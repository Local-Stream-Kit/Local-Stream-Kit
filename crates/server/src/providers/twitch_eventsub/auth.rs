//! The broadcaster's Twitch login.
//!
//! Twitch's device code flow: the app shows a link and a code, and the
//! broadcaster approves it in any browser. Local Stream Kit is registered with
//! Twitch as a "Public" client, so there's no secret: refreshing needs only
//! the client ID. A Public client's refresh token works once and expires after
//! 30 days, so every new one is saved straight away.

use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use protocol::panel::LoginStatus;
use serde::{Deserialize, Serialize};
use twitch_oauth2::tokens::errors::{
    DeviceUserTokenExchangeError, RefreshTokenError, RetrieveTokenError, ValidationError,
};
use twitch_oauth2::{
    AccessToken, ClientId, DeviceUserTokenBuilder, RefreshToken, Scope, TwitchToken, UserToken,
};

/// Refresh the access token (which lasts about 4 hours) when it has less than
/// this left.
const REFRESH_MARGIN: Duration = Duration::from_secs(10 * 60);

/// What the broadcaster is asked for: reading channel point redemptions,
/// followers and hype trains. A saved login without all of them is asked
/// for again.
pub fn scopes() -> Vec<Scope> {
    vec![Scope::ChannelReadRedemptions, Scope::ModeratorReadFollowers, Scope::ChannelReadHypeTrain]
}

/// The tokens file. Who the tokens belong to, their scopes and expiry all come
/// from checking them with Twitch, so only the tokens are kept.
#[derive(Serialize, Deserialize)]
struct Saved {
    access_token: String,
    refresh_token: String,
}

use crate::pipeline::now_ms;
use crate::services::status::StatusHub;

pub struct Auth {
    http: reqwest::Client,
    client_id: ClientId,
    path: PathBuf,
    token: Option<UserToken>,
    status: StatusHub,
    /// Why the next login is needed, shown with its code.
    reason: Option<String>,
}

impl Auth {
    pub fn new(http: reqwest::Client, client_id: &str, path: PathBuf, status: StatusHub) -> Auth {
        Auth {
            http,
            client_id: ClientId::new(client_id.to_string()),
            path,
            token: None,
            status,
            reason: None,
        }
    }

    /// Logs out: tells Twitch to revoke the token (best effort) and forgets
    /// the login.
    pub async fn logout(&mut self) {
        if let Some(token) = &self.token
            && let Err(error) = token.access_token.revoke_token(&self.http, &self.client_id).await
        {
            tracing::warn!("couldn't revoke the Twitch token: {error}");
        }
        self.forget();
        self.reason = None;
        tracing::info!("logged out of Twitch");
    }

    pub fn logged_in(&self) -> bool {
        self.token.is_some()
    }

    /// A token that's valid right now: the one in memory, else the saved one,
    /// else a new login, waiting for the broadcaster to approve it. An error
    /// means Twitch couldn't be reached or answered oddly; try again later.
    pub async fn token(&mut self) -> anyhow::Result<UserToken> {
        loop {
            match &self.token {
                Some(token) if token.expires_in() >= REFRESH_MARGIN => return Ok(token.clone()),
                Some(_) => self.refresh().await?,
                None => {
                    self.status.set_login(LoginStatus::Checking);
                    let token = match self.load().await? {
                        Some(token) => token,
                        None => self.login().await?,
                    };
                    self.save(&token)?;
                    self.status.set_login(LoginStatus::LoggedIn {
                        login: token.login.to_string(),
                        listening: false,
                        follows: false,
                        hype_train: false,
                    });
                    self.token = Some(token);
                }
            }
        }
    }

    /// Twitch asks apps to check their token at least once an hour. A token
    /// that's no longer accepted is refreshed, or forgotten if that fails too.
    pub async fn check(&mut self) -> anyhow::Result<()> {
        let Some(token) = &self.token else { return Ok(()) };
        match token.validate_token(&self.http).await {
            Ok(_) => Ok(()),
            Err(ValidationError::NotAuthorized) => self.refresh().await,
            Err(error) => Err(error).context("couldn't check the Twitch login"),
        }
    }

    /// Drops the login, in memory and on disk, so the next `token()` asks the
    /// broadcaster to log in again.
    pub fn forget(&mut self) {
        self.token = None;
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != ErrorKind::NotFound
        {
            tracing::warn!("couldn't delete {}: {error}", self.path.display());
        }
    }

    async fn refresh(&mut self) -> anyhow::Result<()> {
        // A failed refresh leaves the token without its refresh token, so it's
        // never put back: the next `token()` reloads the saved one instead.
        let Some(mut token) = self.token.take() else { return Ok(()) };
        match token.refresh_token(&self.http).await {
            Ok(()) => {
                self.save(&token)?;
                self.token = Some(token);
                Ok(())
            }
            Err(RefreshTokenError::RequestError(error)) => {
                Err(error).context("couldn't reach Twitch to refresh the login")
            }
            Err(error) => {
                tracing::warn!("Twitch refused to refresh the login ({error}); it has to be done again");
                self.reason = Some("The saved login stopped working.".into());
                self.forget();
                Ok(())
            }
        }
    }

    /// The saved login, checked with Twitch (and refreshed if its access token
    /// expired while the app wasn't running). `None` if there isn't one or
    /// Twitch no longer accepts it.
    async fn load(&mut self) -> anyhow::Result<Option<UserToken>> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("couldn't read {}", self.path.display()));
            }
        };
        let saved: Saved = match serde_json::from_str(&text) {
            Ok(saved) => saved,
            Err(error) => {
                tracing::warn!("{} is unreadable ({error}); log in again", self.path.display());
                return Ok(None);
            }
        };
        let result = UserToken::from_existing_or_refresh_token(
            &self.http,
            AccessToken::new(saved.access_token),
            RefreshToken::new(saved.refresh_token),
            self.client_id.clone(),
            None,
        )
        .await;
        let token = match result {
            Ok(token) => token,
            Err(
                RetrieveTokenError::ValidationError { error: ValidationError::Request(error), .. }
                | RetrieveTokenError::RefreshTokenError {
                    error: RefreshTokenError::RequestError(error), ..
                },
            ) => return Err(error).context("couldn't reach Twitch to check the saved login"),
            Err(error) => {
                tracing::warn!("the saved Twitch login no longer works ({error}); log in again");
                self.reason = Some("The saved login stopped working.".into());
                self.forget();
                return Ok(None);
            }
        };
        if !scopes().iter().all(|scope| token.scopes().contains(scope)) {
            tracing::warn!("the saved Twitch login is missing permissions this version needs; log in again");
            self.reason = Some("This version needs more permissions.".into());
            self.forget();
            return Ok(None);
        }
        Ok(Some(token))
    }

    /// Asks the broadcaster to log in and waits until they have. A code that
    /// expires unused is replaced by a new one.
    async fn login(&mut self) -> anyhow::Result<UserToken> {
        loop {
            let mut builder = DeviceUserTokenBuilder::new(self.client_id.clone(), scopes());
            let code = builder.start(&self.http).await.context("couldn't start the Twitch login")?;
            tracing::warn!(
                "Channel points need the broadcaster's Twitch login. Open {} and approve code {} while \
                 logged in to Twitch as the broadcaster (or use the control panel).",
                code.verification_uri,
                code.user_code
            );
            self.status.set_login(LoginStatus::Waiting {
                url: code.verification_uri.clone(),
                code: code.user_code.clone(),
                expires_at_ms: now_ms() + code.expires_in * 1000,
                reason: self.reason.clone(),
            });
            match builder.wait_for_code(&self.http, tokio::time::sleep).await {
                Ok(token) => {
                    tracing::info!(login = %token.login, "logged in to Twitch");
                    self.reason = None;
                    return Ok(token);
                }
                Err(DeviceUserTokenExchangeError::Expired) => {
                    tracing::info!("the login code expired unused; here's a new one");
                }
                Err(error) => return Err(error).context("the Twitch login failed"),
            }
        }
    }

    fn save(&self, token: &UserToken) -> anyhow::Result<()> {
        let Some(refresh_token) = &token.refresh_token else { bail!("Twitch sent no refresh token") };
        let saved = Saved {
            access_token: token.access_token.secret().to_string(),
            refresh_token: refresh_token.secret().to_string(),
        };
        let json = serde_json::to_vec_pretty(&saved)?;
        // Written to a temporary file and renamed, so a crash mid-write can't
        // lose the (single-use) refresh token.
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, json)
            .and_then(|()| std::fs::rename(&temp, &self.path))
            .with_context(|| format!("couldn't save the Twitch login to {}", self.path.display()))
    }
}
