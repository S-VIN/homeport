//! Common protocol fields are ordinary columns. Open-ended Mihomo maps/lists use
//! typed child rows so new transport options round-trip without serialized blobs.
use rproxy_kernel::{
    Error, Id, Result, Table,
    store::{Store, Tx},
};
fn invalid(m: &str) -> Error {
    Error::msg(m)
}
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{collections::HashMap, sync::Arc};

macro_rules! fields {
    ($($field:ident: $ty:ty => $key:literal),+ $(,)?) => {
        #[derive(Clone,Serialize,Deserialize,Table)]
        #[table(name="outbound_settings")]
        pub struct Fields {
            #[serde(rename="name")] pub id:Id,
            $(#[serde(rename=$key,skip_serializing_if="Option::is_none")] pub $field:Option<$ty>),+
        }
    }
}
fields! {
    uuid:Id=>"uuid", password:String=>"password", login:String=>"username", cipher:String=>"cipher",
    network:String=>"network", tls:bool=>"tls", udp:bool=>"udp", flow:String=>"flow", encryption:String=>"encryption",
    server_name:String=>"servername", sni:String=>"sni", fingerprint:String=>"client-fingerprint",
    skip_cert_verify:bool=>"skip-cert-verify", udp_over_tcp:bool=>"udp-over-tcp", uot_version:u8=>"udp-over-tcp-version"
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Definition {
    #[serde(flatten)]
    pub fields: Fields,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
pub type Definitions = Arc<HashMap<Id, Arc<Definition>>>;
impl Definition {
    pub fn new(id: Id, mut options: Map<String, Value>) -> Result<Self> {
        options.insert("name".into(), json!(id));
        serde_json::from_value(Value::Object(options))
            .map_err(|_| invalid("Invalid field type in proxy options."))
    }
    /// Protocol options for the core (everything except name, type, server, port).
    pub fn options(&self) -> Map<String, Value> {
        let mut v = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        v.remove("name");
        v
    }
}

#[derive(Clone, Serialize, Deserialize, Table)]
#[table(name = "outbound_options")]
struct OptionRow {
    id: Id,
    parent: Id,
    key: String,
    kind: u8,
    text: Option<String>,
    integer: Option<i64>,
    real: Option<f64>,
}
impl OptionRow {
    const NULL: u8 = 0;
    const BOOL: u8 = 1;
    const INT: u8 = 2;
    const REAL: u8 = 3;
    const TEXT: u8 = 4;
    const LIST: u8 = 5;
    const MAP: u8 = 6;
    fn visit(
        parent: Id,
        key: &str,
        value: &Value,
        write: &mut impl FnMut(&Self) -> Result<()>,
    ) -> Result<()> {
        let id = parent.child(key.as_bytes());
        let mut row = Self {
            id,
            parent,
            key: key.into(),
            kind: Self::NULL,
            text: None,
            integer: None,
            real: None,
        };
        row.kind = match value {
            Value::Null => Self::NULL,
            Value::Bool(v) => {
                row.integer = Some((*v).into());
                Self::BOOL
            }
            Value::String(v) => {
                row.text = Some(v.clone());
                Self::TEXT
            }
            Value::Number(n) if n.is_i64() || n.is_u64() => {
                row.integer = Some(
                    n.as_i64()
                        .ok_or_else(|| invalid("Proxy integer exceeds SQLite’s signed range."))?,
                );
                Self::INT
            }
            Value::Number(n) => {
                row.real = n.as_f64();
                Self::REAL
            }
            Value::Array(xs) => {
                for (i, v) in xs.iter().enumerate() {
                    Self::visit(id, &i.to_string(), v, write)?;
                }
                Self::LIST
            }
            Value::Object(xs) => {
                for (k, v) in xs {
                    Self::visit(id, k, v, write)?;
                }
                Self::MAP
            }
        };
        write(&row)
    }
    fn decode(self, children: &mut HashMap<Id, Vec<Self>>, depth: u16) -> Result<Value> {
        if depth > 128 {
            return Err(invalid("Proxy options are nested too deeply."));
        }
        let invalid_row = || invalid("Invalid stored proxy option.");
        Ok(match self.kind {
            Self::NULL => Value::Null,
            Self::BOOL => json!(self.integer.ok_or_else(invalid_row)? != 0),
            Self::INT => json!(self.integer.ok_or_else(invalid_row)?),
            Self::REAL => json!(self.real.ok_or_else(invalid_row)?),
            Self::TEXT => Value::String(self.text.ok_or_else(invalid_row)?),
            Self::LIST => {
                let mut rows = children.remove(&self.id).unwrap_or_default();
                rows.sort_by_key(|r| r.key.parse::<usize>().unwrap_or(usize::MAX));
                let mut list = Vec::with_capacity(rows.len());
                for (i, row) in rows.into_iter().enumerate() {
                    if row.key != i.to_string() {
                        return Err(invalid_row());
                    }
                    list.push(row.decode(children, depth + 1)?);
                }
                Value::Array(list)
            }
            Self::MAP => Value::Object(take_map(self.id, children, depth + 1)?),
            _ => return Err(invalid_row()),
        })
    }
}
fn take_map(
    parent: Id,
    children: &mut HashMap<Id, Vec<OptionRow>>,
    depth: u16,
) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    for row in children.remove(&parent).unwrap_or_default() {
        out.insert(row.key.clone(), row.decode(children, depth)?);
    }
    Ok(out)
}
pub fn register(db: &Store) -> Result<()> {
    db.register::<Fields>()?;
    db.register::<OptionRow>()
}
pub fn read(tx: &Tx<'_>) -> Result<Definitions> {
    let mut children: HashMap<_, Vec<_>> = HashMap::new();
    for row in tx.latest::<OptionRow>(u32::MAX)? {
        children.entry(row.parent).or_default().push(row);
    }
    let mut defs = HashMap::new();
    for fields in tx.latest::<Fields>(u32::MAX)? {
        defs.insert(
            fields.id,
            Arc::new(Definition {
                extra: take_map(fields.id, &mut children, 0)?,
                fields,
            }),
        );
    }
    if !children.is_empty() {
        return Err(invalid("Orphaned proxy options in database."));
    }
    Ok(Arc::new(defs))
}
pub fn sync(tx: &Tx<'_>, old: &Definitions, new: &Definitions) -> Result<()> {
    if Arc::ptr_eq(old, new) {
        return Ok(());
    }
    // Id is derived from the complete outbound config. Only additions/removals can change settings.
    for (id, def) in new.iter().filter(|(id, _)| !old.contains_key(id)) {
        tx.put(&def.fields)?;
        for (k, v) in &def.extra {
            OptionRow::visit(*id, k, v, &mut |r| tx.put(r))?;
        }
    }
    for (id, def) in old.iter().filter(|(id, _)| !new.contains_key(id)) {
        for (k, v) in &def.extra {
            OptionRow::visit(*id, k, v, &mut |r| tx.delete::<OptionRow>(&r.id).map(drop))?;
        }
        tx.delete::<Fields>(id)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_common_columns_and_nested_native_options() {
        let db = Store::open(":memory:").unwrap();
        register(&db).unwrap();
        let id = Id::random();
        let options = json!({"name":id,"password":"example","cipher":"aes-128-gcm","udp":true,"plugin-opts":{"headers":{"X-Foo":["a",null,7,3.5,true]},"empty":[],"mapping":{}}});
        let def: Definition = serde_json::from_value(options.clone()).unwrap();
        assert!(def.extra.contains_key("plugin-opts"));
        assert!(!def.extra.contains_key("password"));
        let empty = Definitions::default();
        let full = Arc::new(HashMap::from([(id, Arc::new(def))]));
        db.transaction(|tx| sync(tx, &empty, &full)).unwrap();
        let restored = db.transaction(read).unwrap();
        assert_eq!(
            serde_json::to_value(restored[&id].as_ref()).unwrap(),
            options
        );
        db.transaction(|tx| sync(tx, &restored, &empty)).unwrap();
        assert!(db.transaction(read).unwrap().is_empty());
    }
}
