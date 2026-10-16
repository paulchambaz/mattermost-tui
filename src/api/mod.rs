//! Read-only Mattermost REST client with automatic re-login.

/// Wire types of the REST API.
pub mod types;

use std::sync::{Mutex, PoisonError};

use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::Credentials;
use types::{
    CategoryList, Channel, ChannelId, ChannelMember, PostId, PostList, Team, TeamId, User, UserId,
};

/// Everything that can go wrong talking to a server.
#[derive(Debug, Error)]
pub enum Error {
    /// The request failed before a usable answer came back.
    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// The server answered with an error status.
    #[error("server answered {status}: {message}")]
    Status { status: u16, message: String },
    /// The server refused the login or password.
    #[error("login refused: {0}")]
    Login(String),
    /// The account has MFA enabled and no valid code was given.
    #[error("a multi-factor code is required")]
    MfaRequired,
    /// A successful login did not carry a session token.
    #[error("the server sent no session token")]
    NoToken,
}

#[derive(Deserialize)]
struct AppError {
    #[serde(default)]
    id: String,
    #[serde(default)]
    message: String,
}

#[derive(Serialize)]
struct LoginBody<'a> {
    login_id: &'a str,
    password: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<&'a str>,
}

/// A connection to one server.
#[derive(Debug)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    credentials: Credentials,
    token: Mutex<String>,
}

impl Client {
    /// Opens a session, logging in with a password or taking the token as is.
    ///
    /// # Errors
    /// Fails when the server is unreachable or refuses the login, with
    /// [`Error::MfaRequired`] when a code is needed.
    pub async fn connect(
        url: &str,
        credentials: Credentials,
        mfa: Option<&str>,
    ) -> Result<Self, Error> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("mm/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let base = format!("{}/api/v4", url.trim_end_matches('/'));
        let token = match &credentials {
            Credentials::Password { login, password } => {
                login_token(&http, &base, login, password, mfa).await?
            }
            Credentials::Token(token) => token.clone(),
        };
        Ok(Self {
            http,
            base,
            credentials,
            token: Mutex::new(token),
        })
    }

    /// The reader's own account.
    ///
    /// # Errors
    /// Fails on network or server errors.
    pub async fn me(&self) -> Result<User, Error> {
        self.fetch(Method::GET, "/users/me", &[], None::<&()>).await
    }

    /// The teams the reader belongs to.
    ///
    /// # Errors
    /// Fails on network or server errors.
    pub async fn teams(&self) -> Result<Vec<Team>, Error> {
        self.fetch(Method::GET, "/users/me/teams", &[], None::<&()>)
            .await
    }

    /// The reader's channels in a team, including direct and group messages.
    ///
    /// # Errors
    /// Fails on network or server errors.
    pub async fn channels(&self, team: &TeamId) -> Result<Vec<Channel>, Error> {
        let path = format!("/users/me/teams/{}/channels", team.0);
        self.fetch(Method::GET, &path, &[], None::<&()>).await
    }

    /// The reader's memberships in a team, which hold the unread counts.
    ///
    /// # Errors
    /// Fails on network or server errors.
    pub async fn members(&self, team: &TeamId) -> Result<Vec<ChannelMember>, Error> {
        let path = format!("/users/me/teams/{}/channels/members", team.0);
        self.fetch(Method::GET, &path, &[], None::<&()>).await
    }

    /// The sidebar categories of a team, which say which channels are shown.
    ///
    /// # Errors
    /// Fails on network or server errors, and on servers older than 5.26.
    pub async fn categories(&self, team: &TeamId) -> Result<CategoryList, Error> {
        let path = format!("/users/me/teams/{}/channels/categories", team.0);
        self.fetch(Method::GET, &path, &[], None::<&()>).await
    }

    /// Marks a channel as viewed, which is the only thing besides logging in that changes the server.
    ///
    /// # Errors
    /// Fails on network or server errors.
    pub async fn view(&self, channel: &ChannelId, prev: Option<&ChannelId>) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Body<'a> {
            channel_id: &'a str,
            prev_channel_id: &'a str,
        }
        let body = Body {
            channel_id: &channel.0,
            prev_channel_id: prev.map_or("", |p| p.0.as_str()),
        };
        let _: serde_json::Value = self
            .fetch(Method::POST, "/channels/members/me/view", &[], Some(&body))
            .await?;
        Ok(())
    }

    /// A page of posts, newest first, optionally those older than `before`.
    ///
    /// # Errors
    /// Fails on network or server errors.
    pub async fn posts(
        &self,
        channel: &ChannelId,
        before: Option<&PostId>,
        per_page: u32,
    ) -> Result<PostList, Error> {
        let mut query = vec![("per_page", per_page.to_string())];
        if let Some(before) = before {
            query.push(("before", before.0.clone()));
        }
        let path = format!("/channels/{}/posts", channel.0);
        self.fetch(Method::GET, &path, &query, None::<&()>).await
    }

    /// Resolves user ids into accounts.
    ///
    /// # Errors
    /// Fails on network or server errors.
    pub async fn users(&self, ids: &[UserId]) -> Result<Vec<User>, Error> {
        self.fetch(Method::POST, "/users/ids", &[], Some(ids)).await
    }

    async fn fetch<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&(impl Serialize + ?Sized)>,
    ) -> Result<T, Error> {
        let url = format!("{}{path}", self.base);
        let build = |token: String| {
            let req = self
                .http
                .request(method.clone(), &url)
                .query(query)
                .bearer_auth(token);
            match body {
                Some(body) => req.json(body),
                None => req,
            }
        };
        let mut resp = send(build(self.token())).await?;
        if resp.status() == StatusCode::UNAUTHORIZED && self.relogin().await? {
            resp = send(build(self.token())).await?;
        }
        if resp.status().is_success() {
            return Ok(resp.json().await?);
        }
        Err(status_error(resp).await)
    }

    /// Logs in again after the session expired; false when there is no password to do it with.
    ///
    /// # Errors
    /// Fails when the server refuses the login.
    pub async fn relogin(&self) -> Result<bool, Error> {
        let Credentials::Password { login, password } = &self.credentials else {
            return Ok(false);
        };
        let token = login_token(&self.http, &self.base, login, password, None).await?;
        *self.token.lock().unwrap_or_else(PoisonError::into_inner) = token;
        Ok(true)
    }

    /// The address of the event stream.
    pub fn ws_url(&self) -> String {
        let (secure, rest) = match self.base.strip_prefix("https://") {
            Some(rest) => (true, rest),
            None => (false, self.base.trim_start_matches("http://")),
        };
        format!("{}://{rest}/websocket", if secure { "wss" } else { "ws" })
    }

    /// The current session token.
    pub fn token(&self) -> String {
        self.token
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

async fn send(req: RequestBuilder) -> Result<Response, Error> {
    Ok(req.send().await?)
}

async fn status_error(resp: Response) -> Error {
    let status = resp.status().as_u16();
    let message = match resp.json::<AppError>().await {
        Ok(e) => e.message,
        Err(_) => String::new(),
    };
    Error::Status { status, message }
}

async fn login_token(
    http: &reqwest::Client,
    base: &str,
    login: &str,
    password: &str,
    mfa: Option<&str>,
) -> Result<String, Error> {
    let body = LoginBody {
        login_id: login,
        password,
        token: mfa,
    };
    let resp = http
        .post(format!("{base}/users/login"))
        .json(&body)
        .send()
        .await?;
    if resp.status().is_success() {
        let token = resp.headers().get("token").and_then(|v| v.to_str().ok());
        return token.map(str::to_owned).ok_or(Error::NoToken);
    }
    let status = resp.status();
    let err = resp.json::<AppError>().await.unwrap_or(AppError {
        id: String::new(),
        message: String::new(),
    });
    if err.id.contains("mfa") {
        return Err(Error::MfaRequired);
    }
    if status.is_client_error() {
        return Err(Error::Login(err.message));
    }
    Err(Error::Status {
        status: status.as_u16(),
        message: err.message,
    })
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn password() -> Credentials {
        Credentials::Password {
            login: "paul".into(),
            password: "hunter2".into(),
        }
    }

    fn login_ok(token: &str) -> Mock {
        Mock::given(method("POST"))
            .and(path("/api/v4/users/login"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Token", token)
                    .set_body_json(serde_json::json!({"id": "u1", "username": "paul"})),
            )
    }

    fn me_ok(token: &str) -> Mock {
        Mock::given(method("GET"))
            .and(path("/api/v4/users/me"))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": "u1", "username": "paul"})),
            )
    }

    #[tokio::test]
    async fn login_takes_token_from_header() {
        let server = MockServer::start().await;
        login_ok("tok1").mount(&server).await;
        me_ok("tok1").mount(&server).await;
        let client = Client::connect(&server.uri(), password(), None)
            .await
            .unwrap();
        assert_eq!(client.me().await.unwrap().username, "paul");
    }

    #[tokio::test]
    async fn mfa_is_reported() {
        let server = MockServer::start().await;
        Mock::given(path("/api/v4/users/login"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "id": "mfa.validate_token.authenticate.app_error",
                "message": "Invalid MFA token."
            })))
            .mount(&server)
            .await;
        let err = Client::connect(&server.uri(), password(), None)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::MfaRequired));
    }

    #[tokio::test]
    async fn bad_password_is_refused() {
        let server = MockServer::start().await;
        Mock::given(path("/api/v4/users/login"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "id": "api.user.login.invalid_credentials_email_username",
                "message": "Enter a valid email or username."
            })))
            .mount(&server)
            .await;
        let err = Client::connect(&server.uri(), password(), None)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Login(_)));
    }

    #[tokio::test]
    async fn expired_session_logs_in_again() {
        let server = MockServer::start().await;
        login_ok("tok1").up_to_n_times(1).mount(&server).await;
        login_ok("tok2").mount(&server).await;
        Mock::given(path("/api/v4/users/me"))
            .and(header("authorization", "Bearer tok1"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        me_ok("tok2").mount(&server).await;
        let client = Client::connect(&server.uri(), password(), None)
            .await
            .unwrap();
        assert_eq!(client.me().await.unwrap().username, "paul");
    }

    #[tokio::test]
    async fn second_rejection_is_an_error() {
        let server = MockServer::start().await;
        login_ok("tok1").mount(&server).await;
        Mock::given(path("/api/v4/users/me"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let client = Client::connect(&server.uri(), password(), None)
            .await
            .unwrap();
        assert!(matches!(
            client.me().await,
            Err(Error::Status { status: 401, .. })
        ));
    }

    #[tokio::test]
    async fn token_is_used_without_login() {
        let server = MockServer::start().await;
        me_ok("pat").mount(&server).await;
        let client = Client::connect(&server.uri(), Credentials::Token("pat".into()), None)
            .await
            .unwrap();
        assert_eq!(client.me().await.unwrap().id, UserId("u1".into()));
    }

    #[tokio::test]
    async fn view_sends_the_channel_and_the_previous_one() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v4/channels/members/me/view"))
            .and(wiremock::matchers::body_json(
                serde_json::json!({"channel_id": "c2", "prev_channel_id": "c1"}),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"status": "OK", "last_viewed_at_times": {}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = Client::connect(&server.uri(), Credentials::Token("t".into()), None)
            .await
            .unwrap();
        client
            .view(&ChannelId("c2".into()), Some(&ChannelId("c1".into())))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn users_posts_ids() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v4/users/ids"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([{"id": "u2", "username": "ana"}])),
            )
            .mount(&server)
            .await;
        let client = Client::connect(&server.uri(), Credentials::Token("t".into()), None)
            .await
            .unwrap();
        let users = client.users(&[UserId("u2".into())]).await.unwrap();
        assert_eq!(users[0].username, "ana");
    }
}
