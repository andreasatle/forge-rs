#!/usr/bin/env bash
set -euo pipefail
git config core.hooksPath .githooks
chmod +x .githooks/*
echo "Git hooks configured. Run 'git config core.hooksPath' to verify."
