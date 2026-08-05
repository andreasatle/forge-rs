#!/bin/bash
# Manual, one-shot Vast.ai GPU setup for testing a larger model against a
# forge-rs config's `provider.cheap.unmanaged` server.
#
# This script is standalone tooling. It is not wired into forge-rs's core
# `start` command and does not track or manage instance lifecycle -- it
# rents a GPU, gets a model serving, tunnels it to the exact local port the
# config already expects, and exits. Destroy the instance yourself with
# `forge-rs vast list` / `forge-rs vast destroy <id>` when done.
#
# Usage:
#   ./rent-vast.sh <forge-config.yaml> [min_vram_gb] [max_price_per_hr] [disk_gb] [context_size]
#
# The config's provider.cheap.unmanaged.base_url and .model are the source
# of truth for the local tunnel port and the Hugging Face model to serve --
# nothing here is hardcoded or printed for manual copy-paste.
set -euo pipefail

CONFIG=${1:?"usage: $0 <forge-config.yaml> [min_vram_gb] [max_price_per_hr] [disk_gb] [context_size]"}
MIN_VRAM=${2:-48}
MAX_PRICE=${3:-0.80}
DISK_GB=${4:-50}
CONTEXT_SIZE=${5:-32768}

FORGE_BIN=./target/debug/forge-rs
SSH_KEY=~/.ssh/id_ed25519_vast
SSH_OPTS=(-i "$SSH_KEY" -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10)
# Pre-built via Dockerfile.vast-llama / .github/workflows/build-vast-image.yml
# -- llama.cpp with CUDA is already compiled for all GPU architectures, so
# no build step is needed on the rented instance.
IMAGE="andreasatle/forge-vast-llama:latest"

if [ ! -x "$FORGE_BIN" ]; then
  echo "error: $FORGE_BIN not found -- run 'cargo build' first" >&2
  exit 1
fi
if ! command -v yq >/dev/null 2>&1; then
  echo "error: yq is required (brew install yq)" >&2
  exit 1
fi

# --- source of truth: read target port + model straight from the config ---
BASE_URL=$(yq e '.provider.cheap.unmanaged.base_url' "$CONFIG")
MODEL=$(yq e '.provider.cheap.unmanaged.model' "$CONFIG")
PARALLEL=$(yq e '.provider.cheap.unmanaged.parallel // 1' "$CONFIG")
N_PREDICT=$(yq e '.provider.cheap.unmanaged.n_predict' "$CONFIG")

if [ "$BASE_URL" = "null" ] || [ "$MODEL" = "null" ] || [ "$N_PREDICT" = "null" ]; then
  echo "error: $CONFIG has no provider.cheap.unmanaged block with base_url/model/n_predict" >&2
  exit 1
fi

PORT=$(echo "$BASE_URL" | sed -E 's#^[a-z]+://[^:/]+:([0-9]+).*#\1#')
HF_REPO=${MODEL%%:*}
HF_FILE=${MODEL#*:}
if [ "$HF_REPO" = "$MODEL" ] || [ -z "$HF_FILE" ]; then
  echo "error: provider.cheap.unmanaged.model must be '<hf-repo>:<filename.gguf>', got '$MODEL'" >&2
  exit 1
fi

echo "Config      : $CONFIG"
echo "Local port  : $PORT (from $BASE_URL)"
echo "Model       : $HF_REPO / $HF_FILE"
echo "GPU search  : min ${MIN_VRAM}GB VRAM, max \$${MAX_PRICE}/hr, ${DISK_GB}GB disk"
echo

# --- 1. search + rent (via existing forge-rs vast subcommands) ---
echo "Searching for offers..."
OFFER_IDS=()
while IFS= read -r id; do
  OFFER_IDS+=("$id")
done < <("$FORGE_BIN" vast search --min-ram "$MIN_VRAM" --max-price "$MAX_PRICE" | awk '{print $1}')
if [ ${#OFFER_IDS[@]} -eq 0 ]; then
  echo "error: no offers found meeting the threshold" >&2
  exit 1
fi

INSTANCE_ID=""
for OFFER_ID in "${OFFER_IDS[@]:0:5}"; do
  echo "Renting offer $OFFER_ID..."
  if RENT_OUT=$("$FORGE_BIN" vast rent "$OFFER_ID" --disk "$DISK_GB" --image "$IMAGE" 2>&1); then
    echo "$RENT_OUT"
    INSTANCE_ID=$(echo "$RENT_OUT" | grep -oE 'instance [0-9]+' | awk '{print $2}')
    break
  else
    echo "  failed: $RENT_OUT"
  fi
done
if [ -z "$INSTANCE_ID" ]; then
  echo "error: could not rent any of the top offers found" >&2
  exit 1
fi
echo "Rented instance $INSTANCE_ID"

# --- 2. wait for the instance to come up and expose SSH ---
echo "Waiting for instance $INSTANCE_ID to become reachable..."
SSH_HOST=""
SSH_PORT=""
for _ in $(seq 1 60); do
  LINE=$("$FORGE_BIN" vast list | awk -v id="$INSTANCE_ID" '$1 == id')
  if [ -n "$LINE" ]; then
    CANDIDATE=$(echo "$LINE" | awk '{print $NF}')
    if [[ "$CANDIDATE" == *:* ]]; then
      SSH_HOST=${CANDIDATE%%:*}
      SSH_PORT=${CANDIDATE##*:}
      break
    fi
  fi
  sleep 10
done
if [ -z "$SSH_HOST" ]; then
  echo "error: instance $INSTANCE_ID never exposed an SSH endpoint (check 'forge-rs vast list')" >&2
  exit 1
fi
echo "SSH endpoint: root@$SSH_HOST:$SSH_PORT"

SSH_TARGET="root@$SSH_HOST"
SSH_OPTS+=(-p "$SSH_PORT")

echo "Waiting for SSH to accept connections..."
for _ in $(seq 1 30); do
  if ssh "${SSH_OPTS[@]}" "$SSH_TARGET" true 2>/dev/null; then
    break
  fi
  sleep 10
done
if ! ssh "${SSH_OPTS[@]}" "$SSH_TARGET" true 2>/dev/null; then
  echo "error: SSH never became reachable on $SSH_HOST:$SSH_PORT" >&2
  exit 1
fi

# --- 3. remote setup: download the model, start llama-server ---
# llama.cpp with CUDA is already built into $IMAGE (see Dockerfile.vast-llama)
# -- no compile step runs here.
echo "Downloading the model on the instance..."
REMOTE_SCRIPT=$(cat <<'REMOTE_EOF'
set -euo pipefail

on_error() {
  echo "REMOTE_SETUP_FAILED"
  if [ -f /root/llama-server.log ]; then
    echo "--- tail /root/llama-server.log ---"
    tail -n 60 /root/llama-server.log
  fi
}
trap on_error ERR

mkdir -p /root/models
MODEL_PATH="/root/models/${HF_FILE}"
curl -fL -o "$MODEL_PATH" "https://huggingface.co/${HF_REPO}/resolve/main/${HF_FILE}?download=true"

setsid nohup /root/llama.cpp/build/bin/llama-server \
  -m "$MODEL_PATH" \
  --host 127.0.0.1 \
  --port "$PORT" \
  -ngl 999 \
  -c "$CONTEXT_SIZE" \
  -np "$PARALLEL" \
  -n "$N_PREDICT" \
  </dev/null >/root/llama-server.log 2>&1 &
disown

for _ in $(seq 1 60); do
  if curl -fsS "http://127.0.0.1:${PORT}/health" >/dev/null 2>&1; then
    echo "REMOTE_HEALTH_OK"
    exit 0
  fi
  sleep 5
done
echo "REMOTE_HEALTH_TIMEOUT"
tail -n 80 /root/llama-server.log
exit 1
REMOTE_EOF
)

REMOTE_ENV="HF_REPO=$(printf %q "$HF_REPO") HF_FILE=$(printf %q "$HF_FILE") PORT=$(printf %q "$PORT") CONTEXT_SIZE=$(printf %q "$CONTEXT_SIZE") PARALLEL=$(printf %q "$PARALLEL") N_PREDICT=$(printf %q "$N_PREDICT")"

if ! ssh "${SSH_OPTS[@]}" "$SSH_TARGET" "$REMOTE_ENV bash -s" <<< "$REMOTE_SCRIPT"; then
  echo "error: remote setup or llama-server health check failed (see log above)" >&2
  echo "instance $INSTANCE_ID is still running -- destroy it with: $FORGE_BIN vast destroy $INSTANCE_ID" >&2
  exit 1
fi
echo "Remote llama-server is healthy."

# --- 4. open the SSH tunnel to the exact local port the config expects ---
echo "Opening SSH tunnel: localhost:$PORT -> $SSH_HOST:$PORT..."
ssh "${SSH_OPTS[@]}" -f -N -L "${PORT}:127.0.0.1:${PORT}" "$SSH_TARGET"
TUNNEL_PID=$(pgrep -f "L ${PORT}:127.0.0.1:${PORT}.*$SSH_TARGET" | head -1)

# --- 5. final end-to-end health confirmation through the tunnel ---
sleep 2
if ! curl -fsS "http://127.0.0.1:${PORT}/health"; then
  echo
  echo "error: tunnel is up but localhost:$PORT/health did not respond" >&2
  exit 1
fi
echo
echo "=== SUCCESS ==="
echo "Instance      : $INSTANCE_ID ($SSH_HOST:$SSH_PORT)"
echo "Model         : $HF_REPO / $HF_FILE"
echo "Endpoint      : $BASE_URL  (reachable now, matches $CONFIG as-is)"
echo "Tunnel PID    : ${TUNNEL_PID:-unknown} (kill it to close the tunnel: kill ${TUNNEL_PID:-<pid>})"
echo "When done     : $FORGE_BIN vast destroy $INSTANCE_ID"
