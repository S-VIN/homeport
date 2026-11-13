//! The one shape every subscription format produces. Format plugins turn their
//! documents into [`Proxy`] values; `servers` gives them stable identities.
use crate::{outbound::Definition, Protocol, Server};
use rproxy_kernel::{Error, Id, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Proxy {
    pub name: String,
    #[serde(rename = "type")]
    pub protocol: Protocol,
    pub server: String,
    pub port: u16,
    #[serde(flatten)]
    pub options: Map<String, Value>,
}

impl Proxy {
    /// Validate a mihomo-style proxy map (`name`, `type`, `server`, `port`, options…).
    pub fn from_value(value: Value, position: usize) -> Result<Self> {
        let proxy: Proxy = serde_json::from_value(value).map_err(|_| {
            Error::msg(format!(
                "Invalid or unsupported proxy at position {position}."
            ))
        })?;
        if proxy.port == 0 || proxy.server.trim().is_empty() || proxy.name.trim().is_empty() {
            return Err(Error::msg("A proxy needs a name, host and nonzero port."));
        }
        if restricted(&Value::Object(proxy.options.clone())) {
            return Err(Error::msg(
                "Provider requests a local file, interface or chained proxy.",
            ));
        }
        Ok(proxy)
    }
    /// Identity: the source plus the complete outbound config except its display name.
    /// Changing credentials or transport creates a new server; renaming does not.
    pub(crate) fn normalize(mut self, source: Id) -> Result<(Server, Definition)> {
        let name = std::mem::take(&mut self.name);
        let encoded =
            serde_json::to_vec(&self).map_err(|_| Error::msg("Invalid proxy definition"))?;
        let id = source.child(&encoded);
        let definition = Definition::new(id, self.options)?;
        Ok((
            Server {
                id,
                source,
                name,
                protocol: self.protocol,
                address: self.server,
                port: self.port,
            },
            definition,
        ))
    }
}

/// Options that would let a provider touch local files, interfaces or chain proxies.
fn restricted(value: &Value) -> bool {
    match value {
        Value::Object(m) => m.iter().any(|(k, v)| {
            matches!(
                k.as_str(),
                "dialer-proxy"
                    | "interface-name"
                    | "routing-mark"
                    | "certificate"
                    | "private-key-path"
                    | "ca"
                    | "ca-str"
                    | "client-cert"
                    | "client-key"
                    | "ip-version-interface"
                    | "private-key-file"
                    | "ech-opts-file"
                    | "plugin-path"
            ) || restricted(v)
        }),
        Value::Array(items) => items.iter().any(restricted),
        _ => false,
    }
}
