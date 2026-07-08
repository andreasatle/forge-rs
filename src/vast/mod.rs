//! Vast.ai GPU instance management via their REST API.
//!
//! Wraps the subset of the `https://console.vast.ai/api/v0` REST API needed
//! to search for GPU offers, rent an instance, list running instances, and
//! tear them down.

use std::time::Duration;

use serde::{Deserialize, Serialize};

const BASE_URL: &str = "https://console.vast.ai/api/v0";
const BASE_URL_V1: &str = "https://console.vast.ai/api/v1";
const DEFAULT_IMAGE: &str = "nvidia/cuda:12.1.0-base-ubuntu22.04";

/// A rentable GPU offer returned by a search.
#[derive(Debug, Clone, PartialEq)]
pub struct VastOffer {
    /// Offer id, passed to [`VastClient::create_instance`] to rent it.
    pub id: u64,
    /// GPU model name, e.g. `"RTX_4090"`.
    pub gpu_name: String,
    /// GPU memory in GB.
    pub gpu_ram_gb: f64,
    /// Rental price in USD per hour.
    pub price_per_hour: f64,
    /// Number of GPUs included in the offer.
    pub num_gpus: u32,
}

/// A rented Vast.ai instance.
#[derive(Debug, Clone, PartialEq)]
pub struct VastInstance {
    /// Instance id.
    pub id: u64,
    /// Vast.ai lifecycle status (e.g. `"running"`, `"loading"`).
    pub status: String,
    /// SSH host, once the instance has finished starting.
    pub ssh_host: Option<String>,
    /// SSH port, once the instance has finished starting.
    pub ssh_port: Option<u16>,
    /// GPU model name.
    pub gpu_name: String,
}

/// An error from a Vast.ai API call.
#[derive(Debug)]
pub enum VastError {
    /// `VAST_API_KEY` is not set in the environment or a `.env` file.
    ApiKeyMissing,
    /// The HTTP request failed, or the API returned an error status.
    Http(String),
    /// The API response body could not be parsed.
    Parse(String),
}

impl std::fmt::Display for VastError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VastError::ApiKeyMissing => write!(f, "VAST_API_KEY is not set"),
            VastError::Http(message) => write!(f, "Vast.ai API request failed: {message}"),
            VastError::Parse(message) => write!(f, "Vast.ai API response parse error: {message}"),
        }
    }
}

impl std::error::Error for VastError {}

/// Client for the Vast.ai REST API.
pub struct VastClient {
    api_key: String,
    agent: ureq::Agent,
}

impl VastClient {
    /// Build a client, reading `VAST_API_KEY` from the environment.
    ///
    /// Loads a `.env` file from the current directory first, if present.
    pub fn new() -> Result<Self, VastError> {
        dotenvy::dotenv().ok();
        let api_key = std::env::var("VAST_API_KEY").map_err(|_| VastError::ApiKeyMissing)?;
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .build();
        Ok(Self { api_key, agent })
    }

    fn authed(&self, request: ureq::Request) -> ureq::Request {
        request.set("Authorization", &format!("Bearer {}", self.api_key))
    }

    /// Search for rentable offers meeting the given GPU memory and price constraints.
    pub fn search_offers(
        &self,
        min_gpu_ram_gb: f64,
        max_price_per_hour: f64,
    ) -> Result<Vec<VastOffer>, VastError> {
        let body = build_search_request(min_gpu_ram_gb, max_price_per_hour);
        let request = self.authed(self.agent.post(&format!("{BASE_URL}/bundles/")));
        let response: SearchOffersResponse = request
            .send_json(&body)
            .map_err(map_ureq_error)?
            .into_json()
            .map_err(|err| VastError::Parse(err.to_string()))?;

        Ok(response.offers.into_iter().map(VastOffer::from).collect())
    }

    /// Rent `offer_id` with `disk_gb` of local disk, returning the created instance.
    pub fn create_instance(&self, offer_id: u64, disk_gb: f64) -> Result<VastInstance, VastError> {
        let body = CreateInstanceRequest {
            image: DEFAULT_IMAGE.to_string(),
            disk: disk_gb,
        };
        let request = self.authed(self.agent.put(&format!("{BASE_URL}/asks/{offer_id}/")));
        let response: CreateInstanceResponse = request
            .send_json(&body)
            .map_err(map_ureq_error)?
            .into_json()
            .map_err(|err| VastError::Parse(err.to_string()))?;

        self.get_instance(response.new_contract)
    }

    /// List all instances owned by the account.
    pub fn list_instances(&self) -> Result<Vec<VastInstance>, VastError> {
        let request = self.authed(self.agent.get(&format!("{BASE_URL_V1}/instances/")));
        let response: ListInstancesResponse = request
            .call()
            .map_err(map_ureq_error)?
            .into_json()
            .map_err(|err| VastError::Parse(err.to_string()))?;

        Ok(response
            .instances
            .into_iter()
            .map(VastInstance::from)
            .collect())
    }

    /// Permanently destroy an instance.
    pub fn destroy_instance(&self, instance_id: u64) -> Result<(), VastError> {
        let request = self.authed(
            self.agent
                .delete(&format!("{BASE_URL_V1}/instances/{instance_id}/")),
        );
        request.call().map_err(map_ureq_error)?;
        Ok(())
    }

    /// Resolve the `ssh://` URL for a running instance.
    pub fn ssh_url(&self, instance_id: u64) -> Result<String, VastError> {
        let instance = self.get_instance(instance_id)?;
        resolve_ssh_url(instance_id, &instance)
    }

    fn get_instance(&self, instance_id: u64) -> Result<VastInstance, VastError> {
        let request = self.authed(
            self.agent
                .get(&format!("{BASE_URL}/instances/{instance_id}/")),
        );
        let response: ShowInstanceResponse = request
            .call()
            .map_err(map_ureq_error)?
            .into_json()
            .map_err(|err| VastError::Parse(err.to_string()))?;

        Ok(VastInstance::from(response.instances))
    }
}

fn map_ureq_error(err: ureq::Error) -> VastError {
    match err {
        ureq::Error::Status(status, response) => {
            let body = response.into_string().unwrap_or_default();
            VastError::Http(format!("HTTP {status}: {body}"))
        }
        ureq::Error::Transport(transport) => {
            VastError::Http(format!("connection error: {transport}"))
        }
    }
}

/// Build the ssh:// URL for an instance, or a descriptive error if it isn't ready yet.
fn resolve_ssh_url(instance_id: u64, instance: &VastInstance) -> Result<String, VastError> {
    match (&instance.ssh_host, instance.ssh_port) {
        (Some(host), Some(port)) => Ok(format!("ssh://root@{host}:{port}")),
        _ => Err(VastError::Http(format!(
            "instance {instance_id} has no SSH endpoint yet (status: {})",
            instance.status
        ))),
    }
}

fn build_search_request(min_gpu_ram_gb: f64, max_price_per_hour: f64) -> SearchOffersRequest {
    SearchOffersRequest {
        limit: 100,
        kind: "ondemand".to_string(),
        gpu_ram: GteFilter {
            gte: min_gpu_ram_gb * 1024.0,
        },
        dph_total: LteFilter {
            lte: max_price_per_hour,
        },
        rentable: EqFilter { eq: true },
    }
}

#[derive(Serialize)]
struct GteFilter {
    gte: f64,
}

#[derive(Serialize)]
struct LteFilter {
    lte: f64,
}

#[derive(Serialize)]
struct EqFilter<T> {
    eq: T,
}

#[derive(Serialize)]
struct SearchOffersRequest {
    limit: u32,
    #[serde(rename = "type")]
    kind: String,
    gpu_ram: GteFilter,
    dph_total: LteFilter,
    rentable: EqFilter<bool>,
}

#[derive(Deserialize)]
struct SearchOffersResponse {
    offers: Vec<RawOffer>,
}

#[derive(Deserialize)]
struct RawOffer {
    id: u64,
    gpu_name: String,
    gpu_ram: f64,
    dph_total: f64,
    num_gpus: u32,
}

impl From<RawOffer> for VastOffer {
    fn from(raw: RawOffer) -> Self {
        VastOffer {
            id: raw.id,
            gpu_name: raw.gpu_name,
            gpu_ram_gb: raw.gpu_ram / 1024.0,
            price_per_hour: raw.dph_total,
            num_gpus: raw.num_gpus,
        }
    }
}

#[derive(Serialize)]
struct CreateInstanceRequest {
    image: String,
    disk: f64,
}

#[derive(Deserialize)]
struct CreateInstanceResponse {
    new_contract: u64,
}

#[derive(Deserialize)]
struct RawInstance {
    id: u64,
    actual_status: Option<String>,
    ssh_host: Option<String>,
    ssh_port: Option<u16>,
    gpu_name: String,
}

impl From<RawInstance> for VastInstance {
    fn from(raw: RawInstance) -> Self {
        VastInstance {
            id: raw.id,
            status: raw.actual_status.unwrap_or_else(|| "unknown".to_string()),
            ssh_host: raw.ssh_host,
            ssh_port: raw.ssh_port,
            gpu_name: raw.gpu_name,
        }
    }
}

#[derive(Deserialize)]
struct ListInstancesResponse {
    instances: Vec<RawInstance>,
}

#[derive(Deserialize)]
struct ShowInstanceResponse {
    instances: RawInstance,
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
