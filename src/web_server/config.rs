use std::env;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use axum::http::Uri;

use super::rate_limit::WebRateLimitConfig;
use crate::release_identity::{
    DEVELOPMENT_RELEASE_LABEL, compiled_build_profile, current_release_identity,
};
use crate::web_endpoint_adapters::{DEFAULT_WEB_ADMISSION_TIMEOUT, WebEndpointConfig};
use crate::web_identity::{WebTokenKeyring, WebTokenLifetimes, WebTokenSigningKey};
use crate::web_room::WebRoomServiceConfig;

const DEFAULT_HTTP_BIND: &str = "0.0.0.0:8080";
const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 16 * 1024;
const DEFAULT_MAX_TRANSPORT_SESSIONS: usize = 4_096;
const MAX_ALLOWED_ORIGINS: usize = 32;
const MAX_TRUSTED_PROXY_IPS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebDeploymentMode {
    Development,
    Production,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebTransportListenerConfig {
    pub bind: SocketAddr,
    pub certificate_pem: PathBuf,
    pub private_key_pem: PathBuf,
}

#[derive(Clone, Debug)]
pub struct WebServerConfig {
    pub deployment: WebDeploymentMode,
    pub http_bind: SocketAddr,
    pub webtransport: Option<WebTransportListenerConfig>,
    pub public_websocket_url: String,
    pub public_webtransport_url: Option<String>,
    pub allowed_origins: Vec<String>,
    /// Exact socket peers whose `X-Forwarded-For` chain may be trusted.
    /// Untrusted peers cannot influence rate-limit identity with headers.
    pub trusted_proxy_ips: Vec<IpAddr>,
    pub token_keyring: WebTokenKeyring,
    pub room: WebRoomServiceConfig,
    pub endpoint: WebEndpointConfig,
    pub rate_limit: WebRateLimitConfig,
    pub admission_timeout: Duration,
    pub maximum_request_body_bytes: usize,
    pub maximum_transport_sessions: usize,
}

impl WebServerConfig {
    pub fn validate(&self) -> Result<(), WebServerConfigError> {
        validate_deployment_build(
            self.deployment,
            compiled_build_profile(),
            current_release_identity().release_label,
        )?;
        if self.deployment == WebDeploymentMode::Production && self.http_bind.port() == 0 {
            return Err(WebServerConfigError::InvalidHttpBind);
        }
        if self.allowed_origins.is_empty() || self.allowed_origins.len() > MAX_ALLOWED_ORIGINS {
            return Err(WebServerConfigError::InvalidAllowedOrigins);
        }
        for origin in &self.allowed_origins {
            validate_origin(origin, self.deployment)?;
        }
        if self.trusted_proxy_ips.len() > MAX_TRUSTED_PROXY_IPS
            || self.trusted_proxy_ips.iter().any(|address| {
                address.is_unspecified()
                    || address.is_multicast()
                    || self
                        .trusted_proxy_ips
                        .iter()
                        .filter(|candidate| *candidate == address)
                        .count()
                        != 1
            })
        {
            return Err(WebServerConfigError::InvalidTrustedProxyIps);
        }
        validate_public_url(
            &self.public_websocket_url,
            "ws",
            "wss",
            "/v1/connect/ws",
            self.deployment,
        )?;
        match (&self.webtransport, &self.public_webtransport_url) {
            (Some(_), Some(url)) => {
                validate_public_url(url, "https", "https", "/v1/connect/wt", self.deployment)?;
            }
            (None, None) if self.deployment == WebDeploymentMode::Development => {}
            _ => return Err(WebServerConfigError::InvalidWebTransportConfiguration),
        }
        if self.deployment == WebDeploymentMode::Production && self.webtransport.is_none() {
            return Err(WebServerConfigError::WebTransportRequiredInProduction);
        }
        if self.deployment == WebDeploymentMode::Production
            && self
                .webtransport
                .as_ref()
                .is_some_and(|transport| transport.bind.port() == 0)
        {
            return Err(WebServerConfigError::InvalidWebTransportConfiguration);
        }
        self.room
            .validate()
            .map_err(|_| WebServerConfigError::InvalidRoomConfiguration)?;
        self.endpoint
            .validate()
            .map_err(|_| WebServerConfigError::InvalidEndpointConfiguration)?;
        self.rate_limit
            .validate()
            .map_err(|_| WebServerConfigError::InvalidRateLimitConfiguration)?;
        if !(Duration::from_secs(1)..=Duration::from_secs(15)).contains(&self.admission_timeout)
            || !(1_024..=64 * 1_024).contains(&self.maximum_request_body_bytes)
            || !(1..=100_000).contains(&self.maximum_transport_sessions)
        {
            return Err(WebServerConfigError::InvalidServiceLimits);
        }
        Ok(())
    }

    pub fn from_environment() -> Result<Self, WebServerConfigError> {
        let deployment = match optional_env("AFC_WEB_DEPLOYMENT").as_deref() {
            Some("development") => WebDeploymentMode::Development,
            None | Some("production") => WebDeploymentMode::Production,
            Some(_) => return Err(WebServerConfigError::InvalidDeploymentMode),
        };
        let http_bind = optional_env("AFC_WEB_HTTP_BIND")
            .unwrap_or_else(|| DEFAULT_HTTP_BIND.to_owned())
            .parse()
            .map_err(|_| WebServerConfigError::InvalidHttpBind)?;
        let allowed_origins = required_env("AFC_WEB_ALLOWED_ORIGINS")?
            .split(',')
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
            .map(str::to_owned)
            .collect();
        let trusted_proxy_ips = optional_env("AFC_WEB_TRUSTED_PROXY_IPS")
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|address| !address.is_empty())
                    .map(str::parse)
                    .collect::<Result<Vec<IpAddr>, _>>()
                    .map_err(|_| WebServerConfigError::InvalidTrustedProxyIps)
            })
            .transpose()?
            .unwrap_or_default();
        let current_id = parse_required::<u32>("AFC_WEB_SIGNING_KEY_ID")?;
        let current = WebTokenSigningKey::from_base64_url(
            current_id,
            &required_secret("AFC_WEB_SIGNING_KEY", "AFC_WEB_SIGNING_KEY_FILE")?,
        )
        .map_err(|_| WebServerConfigError::InvalidSigningKey)?;
        let previous_id = optional_env("AFC_WEB_PREVIOUS_SIGNING_KEY_ID");
        let previous_secret = optional_secret(
            "AFC_WEB_PREVIOUS_SIGNING_KEY",
            "AFC_WEB_PREVIOUS_SIGNING_KEY_FILE",
        )?;
        let previous = match (previous_id, previous_secret) {
            (None, None) => None,
            (Some(id), Some(secret)) => Some(
                WebTokenSigningKey::from_base64_url(
                    id.parse()
                        .map_err(|_| WebServerConfigError::InvalidPreviousSigningKey)?,
                    &secret,
                )
                .map_err(|_| WebServerConfigError::InvalidPreviousSigningKey)?,
            ),
            _ => return Err(WebServerConfigError::IncompletePreviousSigningKey),
        };
        let token_keyring = WebTokenKeyring::new(current, previous, WebTokenLifetimes::default())
            .map_err(|_| WebServerConfigError::InvalidSigningKey)?;

        let wt_bind = optional_env("AFC_WEBTRANSPORT_BIND");
        let wt_cert = optional_env("AFC_WEBTRANSPORT_CERT_PEM");
        let wt_key = optional_env("AFC_WEBTRANSPORT_KEY_PEM");
        let webtransport = match (wt_bind, wt_cert, wt_key) {
            (None, None, None) => None,
            (Some(bind), Some(certificate_pem), Some(private_key_pem)) => {
                Some(WebTransportListenerConfig {
                    bind: bind
                        .parse()
                        .map_err(|_| WebServerConfigError::InvalidWebTransportConfiguration)?,
                    certificate_pem: certificate_pem.into(),
                    private_key_pem: private_key_pem.into(),
                })
            }
            _ => return Err(WebServerConfigError::InvalidWebTransportConfiguration),
        };
        let public_websocket_url = required_env("AFC_WEB_PUBLIC_WEBSOCKET_URL")?;
        let public_webtransport_url = optional_env("AFC_WEB_PUBLIC_WEBTRANSPORT_URL");
        let config = Self {
            deployment,
            http_bind,
            webtransport,
            public_websocket_url,
            public_webtransport_url,
            allowed_origins,
            trusted_proxy_ips,
            token_keyring,
            room: WebRoomServiceConfig::default(),
            endpoint: WebEndpointConfig::default(),
            rate_limit: WebRateLimitConfig::default(),
            admission_timeout: DEFAULT_WEB_ADMISSION_TIMEOUT,
            maximum_request_body_bytes: DEFAULT_MAX_REQUEST_BODY_BYTES,
            maximum_transport_sessions: DEFAULT_MAX_TRANSPORT_SESSIONS,
        };
        config.validate()?;
        Ok(config)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebServerConfigError {
    MissingEnvironmentVariable(&'static str),
    InvalidDeploymentMode,
    NonReleaseBuildInProduction,
    DevelopmentBuildInProduction,
    InvalidHttpBind,
    InvalidAllowedOrigins,
    InvalidTrustedProxyIps,
    InvalidPublicUrl,
    InvalidSigningKey,
    InvalidPreviousSigningKey,
    IncompletePreviousSigningKey,
    SecretRead,
    InvalidWebTransportConfiguration,
    WebTransportRequiredInProduction,
    InvalidRoomConfiguration,
    InvalidEndpointConfiguration,
    InvalidRateLimitConfiguration,
    InvalidServiceLimits,
}

impl fmt::Display for WebServerConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid afc-web-server configuration: {self:?}")
    }
}

impl std::error::Error for WebServerConfigError {}

fn validate_deployment_build(
    deployment: WebDeploymentMode,
    build_profile: &str,
    release_label: &str,
) -> Result<(), WebServerConfigError> {
    if deployment != WebDeploymentMode::Production {
        return Ok(());
    }
    if build_profile != "release" {
        return Err(WebServerConfigError::NonReleaseBuildInProduction);
    }
    if release_label == DEVELOPMENT_RELEASE_LABEL {
        return Err(WebServerConfigError::DevelopmentBuildInProduction);
    }
    Ok(())
}

fn required_env(name: &'static str) -> Result<String, WebServerConfigError> {
    optional_env(name).ok_or(WebServerConfigError::MissingEnvironmentVariable(name))
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn required_secret(
    environment_name: &'static str,
    file_environment_name: &'static str,
) -> Result<String, WebServerConfigError> {
    optional_secret(environment_name, file_environment_name)?.ok_or(
        WebServerConfigError::MissingEnvironmentVariable(environment_name),
    )
}

fn optional_secret(
    environment_name: &'static str,
    file_environment_name: &'static str,
) -> Result<Option<String>, WebServerConfigError> {
    let direct = optional_env(environment_name);
    let path = optional_env(file_environment_name);
    match (direct, path) {
        (Some(_), Some(_)) => Err(WebServerConfigError::SecretRead),
        (Some(secret), None) => Ok(Some(secret)),
        (None, Some(path)) => {
            let secret =
                std::fs::read_to_string(path).map_err(|_| WebServerConfigError::SecretRead)?;
            if secret.len() > 1_024 {
                return Err(WebServerConfigError::SecretRead);
            }
            let secret = secret.trim().to_owned();
            if secret.is_empty() {
                return Err(WebServerConfigError::SecretRead);
            }
            Ok(Some(secret))
        }
        (None, None) => Ok(None),
    }
}

fn parse_required<T>(name: &'static str) -> Result<T, WebServerConfigError>
where
    T: std::str::FromStr,
{
    required_env(name)?
        .parse()
        .map_err(|_| WebServerConfigError::InvalidSigningKey)
}

fn validate_origin(
    origin: &str,
    deployment: WebDeploymentMode,
) -> Result<(), WebServerConfigError> {
    if origin == "*" || origin.len() > 2_048 || origin.ends_with('/') {
        return Err(WebServerConfigError::InvalidAllowedOrigins);
    }
    let uri: Uri = origin
        .parse()
        .map_err(|_| WebServerConfigError::InvalidAllowedOrigins)?;
    let scheme = uri
        .scheme_str()
        .ok_or(WebServerConfigError::InvalidAllowedOrigins)?;
    if uri
        .authority()
        .is_none_or(|authority| authority.as_str().contains('@'))
        || !matches!(uri.path(), "" | "/")
        || uri.query().is_some()
        || !matches!(scheme, "http" | "https")
        || (deployment == WebDeploymentMode::Production && scheme != "https")
    {
        return Err(WebServerConfigError::InvalidAllowedOrigins);
    }
    Ok(())
}

fn validate_public_url(
    url: &str,
    development_scheme: &str,
    production_scheme: &str,
    expected_path: &str,
    deployment: WebDeploymentMode,
) -> Result<(), WebServerConfigError> {
    if url.len() > 2_048 {
        return Err(WebServerConfigError::InvalidPublicUrl);
    }
    let uri: Uri = url
        .parse()
        .map_err(|_| WebServerConfigError::InvalidPublicUrl)?;
    let expected = if deployment == WebDeploymentMode::Production {
        production_scheme
    } else {
        development_scheme
    };
    if uri.scheme_str() != Some(expected)
        || uri
            .authority()
            .is_none_or(|authority| authority.as_str().contains('@'))
        || uri.path() != expected_path
        || uri.query().is_some()
    {
        return Err(WebServerConfigError::InvalidPublicUrl);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_rejects_debug_and_mutable_development_artifacts() {
        assert_eq!(
            validate_deployment_build(WebDeploymentMode::Production, "debug", "rc-42"),
            Err(WebServerConfigError::NonReleaseBuildInProduction)
        );
        assert_eq!(
            validate_deployment_build(
                WebDeploymentMode::Production,
                "release",
                DEVELOPMENT_RELEASE_LABEL,
            ),
            Err(WebServerConfigError::DevelopmentBuildInProduction)
        );
        assert!(
            validate_deployment_build(WebDeploymentMode::Production, "release", "rc-42").is_ok()
        );
        assert!(
            validate_deployment_build(
                WebDeploymentMode::Development,
                "debug",
                DEVELOPMENT_RELEASE_LABEL,
            )
            .is_ok()
        );
    }

    #[test]
    fn production_origins_and_transport_urls_are_tls_exact_origins() {
        assert!(validate_origin("https://itch.io", WebDeploymentMode::Production).is_ok());
        assert!(
            validate_origin(
                "https://html-classic.itch.zone",
                WebDeploymentMode::Production
            )
            .is_ok()
        );
        assert!(validate_origin("http://itch.io", WebDeploymentMode::Production).is_err());
        assert!(validate_origin("https://itch.io/", WebDeploymentMode::Production).is_err());
        assert!(validate_origin("*", WebDeploymentMode::Development).is_err());
        assert!(
            validate_public_url(
                "wss://play.example/v1/connect/ws",
                "ws",
                "wss",
                "/v1/connect/ws",
                WebDeploymentMode::Production,
            )
            .is_ok()
        );
        assert!(
            validate_public_url(
                "wss://play.example/wrong",
                "ws",
                "wss",
                "/v1/connect/ws",
                WebDeploymentMode::Production,
            )
            .is_err()
        );
        assert!(
            validate_public_url(
                "wss://user@play.example/v1/connect/ws",
                "ws",
                "wss",
                "/v1/connect/ws",
                WebDeploymentMode::Production,
            )
            .is_err()
        );
    }
}
