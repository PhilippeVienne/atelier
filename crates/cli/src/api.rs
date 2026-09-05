//! Client HTTP minimal vers `api-server` (memes routes REST que le Dashboard,
//! `crates/api-server/src/routes.rs`) : pas de SDK genere, juste les appels
//! dont la CLI a besoin pour `atelier workshops ...`.

use anyhow::{Context, Result};
use atelier_common::{DevcontainerSource, Workshop, WorkshopResources};
use serde::Serialize;

pub struct ApiClient {
    base_url: String,
    access_token: String,
    http: reqwest::Client,
}

impl ApiClient {
    pub fn new(base_url: &str, access_token: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            access_token: access_token.to_string(),
            http: reqwest::Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("api-server a repondu {status}: {body}");
    }

    pub async fn list_workshops(&self) -> Result<Vec<Workshop>> {
        let resp = self
            .http
            .get(self.url("/v1/workshops"))
            .bearer_auth(&self.access_token)
            .send()
            .await
            .context("requete GET /v1/workshops")?;
        Self::check(resp)
            .await?
            .json()
            .await
            .context("reponse /v1/workshops invalide")
    }

    pub async fn get_workshop(&self, name: &str) -> Result<Workshop> {
        let resp = self
            .http
            .get(self.url(&format!("/v1/workshops/{name}")))
            .bearer_auth(&self.access_token)
            .send()
            .await
            .with_context(|| format!("requete GET /v1/workshops/{name}"))?;
        Self::check(resp)
            .await?
            .json()
            .await
            .context("reponse /v1/workshops/{name} invalide")
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_workshop(
        &self,
        name: &str,
        devcontainer: DevcontainerSource,
        resources: WorkshopResources,
        egress_allowlist: Vec<String>,
        tools: Vec<String>,
        owner_group: Option<String>,
    ) -> Result<Workshop> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct CreateWorkshopRequest {
            name: String,
            devcontainer: DevcontainerSource,
            resources: WorkshopResources,
            egress_allowlist: Vec<String>,
            tools: Vec<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            owner_group: Option<String>,
        }

        let body = CreateWorkshopRequest {
            name: name.to_string(),
            devcontainer,
            resources,
            egress_allowlist,
            tools,
            owner_group,
        };

        let resp = self
            .http
            .post(self.url("/v1/workshops"))
            .bearer_auth(&self.access_token)
            .json(&body)
            .send()
            .await
            .context("requete POST /v1/workshops")?;
        Self::check(resp)
            .await?
            .json()
            .await
            .context("reponse POST /v1/workshops invalide")
    }

    pub async fn delete_workshop(&self, name: &str) -> Result<()> {
        let resp = self
            .http
            .delete(self.url(&format!("/v1/workshops/{name}")))
            .bearer_auth(&self.access_token)
            .send()
            .await
            .with_context(|| format!("requete DELETE /v1/workshops/{name}"))?;
        Self::check(resp).await?;
        Ok(())
    }

    pub async fn suspend_workshop(&self, name: &str) -> Result<Workshop> {
        self.post_action(name, "suspend").await
    }

    pub async fn resume_workshop(&self, name: &str) -> Result<Workshop> {
        self.post_action(name, "resume").await
    }

    async fn post_action(&self, name: &str, action: &str) -> Result<Workshop> {
        let resp = self
            .http
            .post(self.url(&format!("/v1/workshops/{name}/{action}")))
            .bearer_auth(&self.access_token)
            .send()
            .await
            .with_context(|| format!("requete POST /v1/workshops/{name}/{action}"))?;
        Self::check(resp)
            .await?
            .json()
            .await
            .with_context(|| format!("reponse /v1/workshops/{name}/{action} invalide"))
    }
}
