// SPDX-License-Identifier: AGPL-3.0-only
//! Teams API client — the Drive "team" scope against `/api/tcb/teams`.
//!
//! A team is the sharing unit for `~/.claude` config: shareable sync keys
//! (agents/commands/skills/rules/CLAUDE.md) are mirrored into team-scoped
//! state so every member's Mnemos pulls them. Same auth/refresh/CAS
//! conventions as the profile-scoped state client.

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::{PushStateRequest, SyncClient, SyncStateResponse, SYNC_API};
use crate::error::IdeError;

#[derive(Debug, Clone, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub member_count: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TeamMember {
    pub profile_id: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default, alias = "name")]
    pub full_name: Option<String>,
}

#[derive(Debug, Serialize)]
struct CreateTeamRequest<'a> {
    name: &'a str,
}

#[derive(Debug, Serialize)]
struct AddMemberRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile_id: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
struct TeamsListResponse {
    teams: Vec<Team>,
}

#[derive(Debug, Deserialize)]
struct MembersListResponse {
    members: Vec<TeamMember>,
}

#[derive(Debug, Deserialize)]
struct TeamStatesResponse {
    states: Vec<SyncStateResponse>,
}

impl SyncClient {
    /// `GET /api/tcb/teams` — teams the caller belongs to.
    pub async fn list_teams(&self) -> Result<Vec<Team>, IdeError> {
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let http = http.clone();
            async move {
                let url = format!("{base_url}{SYNC_API}/teams");
                let resp = http.get(&url).bearer_auth(&token).send().await?;
                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                // Tolerate both `{"teams":[...]}` and a bare array.
                let raw: Value = resp.json().await?;
                let teams = match serde_json::from_value::<TeamsListResponse>(raw.clone()) {
                    Ok(r) => r.teams,
                    Err(_) => serde_json::from_value::<Vec<Team>>(raw)?,
                };
                Ok(teams)
            }
        })
        .await
    }

    /// `POST /api/tcb/teams` — create a team; caller becomes owner.
    pub async fn create_team(&self, name: &str) -> Result<Team, IdeError> {
        let name_owned = name.to_string();
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let name = name_owned.clone();
            let http = http.clone();
            async move {
                let url = format!("{base_url}{SYNC_API}/teams");
                let resp = http
                    .post(&url)
                    .bearer_auth(&token)
                    .json(&CreateTeamRequest { name: &name })
                    .send()
                    .await?;
                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                Ok(resp.json().await?)
            }
        })
        .await
    }

    /// `GET /api/tcb/teams/:id/members`.
    pub async fn list_team_members(&self, team_id: &str) -> Result<Vec<TeamMember>, IdeError> {
        let team_owned = team_id.to_string();
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let team_id = team_owned.clone();
            let http = http.clone();
            async move {
                let url = format!("{base_url}{SYNC_API}/teams/{team_id}/members");
                let resp = http.get(&url).bearer_auth(&token).send().await?;
                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                let raw: Value = resp.json().await?;
                let members = match serde_json::from_value::<MembersListResponse>(raw.clone()) {
                    Ok(r) => r.members,
                    Err(_) => serde_json::from_value::<Vec<TeamMember>>(raw)?,
                };
                Ok(members)
            }
        })
        .await
    }

    /// `POST /api/tcb/teams/:id/members` — owner-only on the server side.
    pub async fn add_team_member(
        &self,
        team_id: &str,
        email: Option<&str>,
        profile_id: Option<&str>,
    ) -> Result<(), IdeError> {
        let team_owned = team_id.to_string();
        let email_owned = email.map(str::to_string);
        let profile_owned = profile_id.map(str::to_string);
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let team_id = team_owned.clone();
            let email = email_owned.clone();
            let profile_id = profile_owned.clone();
            let http = http.clone();
            async move {
                let url = format!("{base_url}{SYNC_API}/teams/{team_id}/members");
                let resp = http
                    .post(&url)
                    .bearer_auth(&token)
                    .json(&AddMemberRequest {
                        email: email.as_deref(),
                        profile_id: profile_id.as_deref(),
                    })
                    .send()
                    .await?;
                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                Ok(())
            }
        })
        .await
    }

    /// `GET /api/tcb/teams/:id/state` — pull every team-scoped key.
    pub async fn pull_team_all(&self, team_id: &str) -> Result<Vec<SyncStateResponse>, IdeError> {
        let team_owned = team_id.to_string();
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let team_id = team_owned.clone();
            let http = http.clone();
            async move {
                let url = format!("{base_url}{SYNC_API}/teams/{team_id}/state");
                let resp = http.get(&url).bearer_auth(&token).send().await?;
                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                let raw: Value = resp.json().await?;
                let states = match serde_json::from_value::<TeamStatesResponse>(raw.clone()) {
                    Ok(r) => r.states,
                    Err(_) => serde_json::from_value::<Vec<SyncStateResponse>>(raw)?,
                };
                Ok(states)
            }
        })
        .await
    }

    /// `GET /api/tcb/teams/:id/state/:key` — `Ok(None)` on 404 like `get_state`.
    pub async fn get_team_state(
        &self,
        team_id: &str,
        key: &str,
    ) -> Result<Option<SyncStateResponse>, IdeError> {
        let team_owned = team_id.to_string();
        let key_owned = key.to_string();
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let team_id = team_owned.clone();
            let key = key_owned.clone();
            let http = http.clone();
            async move {
                let url = format!("{base_url}{SYNC_API}/teams/{team_id}/state/{key}");
                let resp = http.get(&url).bearer_auth(&token).send().await?;
                let status = resp.status();
                if status == StatusCode::NOT_FOUND {
                    return Ok(None);
                }
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                let parsed: SyncStateResponse = resp.json().await?;
                Ok(Some(parsed))
            }
        })
        .await
    }

    /// `POST /api/tcb/teams/:id/state` — LWW when `expected_version` is None,
    /// CAS otherwise (same envelope as the profile-scoped `put_state`).
    pub async fn put_team_state(
        &self,
        team_id: &str,
        key: &str,
        value: &Value,
        expected_version: Option<i64>,
    ) -> Result<SyncStateResponse, IdeError> {
        let team_owned = team_id.to_string();
        let key_owned = key.to_string();
        let value_owned = value.clone();
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let team_id = team_owned.clone();
            let key = key_owned.clone();
            let value = value_owned.clone();
            let http = http.clone();
            async move {
                let url = format!("{base_url}{SYNC_API}/teams/{team_id}/state");
                let resp = http
                    .post(&url)
                    .bearer_auth(&token)
                    .json(&PushStateRequest {
                        key: &key,
                        value: &value,
                        expected_version,
                    })
                    .send()
                    .await?;
                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                Ok(resp.json().await?)
            }
        })
        .await
    }
}
