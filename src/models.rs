//! The models endpoint.
//!
//! Listing the models is also how a client checks that its API key works and
//! opens its connection before the first real call: the request carries no
//! body, so a key that is rejected is rejected cheaply.
//!
//! This module holds what the endpoint answers with. A model card carries
//! three strings; members the API adds later are ignored by the decoder and
//! stay readable in the raw body.

use std::fmt;

use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode, Uri};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, IgnoredAny, MapAccess, Visitor},
    ser::SerializeStruct,
};

use crate::{
    codec,
    de::{KeyIn, invalid_response},
    error::Error,
    response::ResponseMeta,
};

/// One model the account can use.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct ModelMetadata {
    name: String,
    description: String,
    release_date: String,
}

impl ModelMetadata {
    /// The name or alias a request's `model` accepts, such as `jev-latest`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the model is for.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The release date, as the API writes it: `YYYY-MM-DD`.
    #[must_use]
    pub fn release_date(&self) -> &str {
        &self.release_date
    }
}

/// The models available to the account, with the HTTP response they came in.
///
/// Serializing it writes `models` only; the HTTP metadata is runtime state,
/// not part of the API payload.
#[derive(Debug, Clone, PartialEq)]
pub struct ListModelsResponse {
    models: Vec<ModelMetadata>,
    meta: ResponseMeta,
}

impl ListModelsResponse {
    /// The models, in the order the API listed them.
    #[must_use]
    pub fn models(&self) -> &[ModelMetadata] {
        &self.models
    }

    /// The status, headers and raw body of the HTTP response.
    #[must_use]
    pub fn meta(&self) -> &ResponseMeta {
        &self.meta
    }

    /// Gives up everything but the models.
    #[must_use]
    pub fn into_models(self) -> Vec<ModelMetadata> {
        self.models
    }
}

impl Serialize for ListModelsResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut out = serializer.serialize_struct("ListModelsResponse", 1)?;
        out.serialize_field("models", &self.models)?;
        out.end()
    }
}

impl<'de> Deserialize<'de> for ModelMetadata {
    /// Reads a model card from an object; members this version does not know
    /// are ignored.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(CardVisitor)
    }
}

/// Reads a model card as an object only. serde's derived reader would also
/// take a JSON array positionally, which the API's schema does not allow.
struct CardVisitor;

impl<'de> Visitor<'de> for CardVisitor {
    type Value = ModelMetadata;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a model card")
    }

    fn visit_map<M>(self, mut map: M) -> Result<ModelMetadata, M::Error>
    where
        M: MapAccess<'de>,
    {
        let (mut name, mut description, mut release_date) = (None, None, None);
        while let Some(index) =
            map.next_key_seed(KeyIn(&["name", "description", "release_date"]))?
        {
            match index {
                Some(0) => name = Some(map.next_value()?),
                Some(1) => description = Some(map.next_value()?),
                Some(2) => release_date = Some(map.next_value()?),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(ModelMetadata {
            name: name.ok_or_else(|| de::Error::missing_field("name"))?,
            description: description.ok_or_else(|| de::Error::missing_field("description"))?,
            release_date: release_date.ok_or_else(|| de::Error::missing_field("release_date"))?,
        })
    }
}

/// The body of a models response.
struct ModelList {
    models: Vec<ModelMetadata>,
}

impl<'de> Deserialize<'de> for ModelList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(ModelListVisitor)
    }
}

struct ModelListVisitor;

impl<'de> Visitor<'de> for ModelListVisitor {
    type Value = ModelList;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a list of models")
    }

    fn visit_map<M>(self, mut map: M) -> Result<ModelList, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut models = None;
        while let Some(index) = map.next_key_seed(KeyIn(&["models"]))? {
            match index {
                Some(_) => models = Some(map.next_value()?),
                None => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(ModelList { models: models.ok_or_else(|| de::Error::missing_field("models"))? })
    }
}

/// Decodes the body of a successful models response.
///
/// # Errors
///
/// Returns [`ErrorKind::ResponseValidation`](crate::ErrorKind::ResponseValidation)
/// when the body does not have the documented shape; its field path names the
/// card and the member, such as `models[1].name`.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the models resource that calls this is written in phase 2")
)]
pub(crate) fn decode_list_models(
    body: Bytes,
    status: StatusCode,
    headers: HeaderMap,
    endpoint: Option<(&Method, &Uri)>,
) -> Result<ListModelsResponse, Error> {
    let meta = ResponseMeta::new(status, headers, body);
    let decoded = codec::decode::<ModelList>(meta.raw_body());
    match decoded {
        Ok(ModelList { models }) => Ok(ListModelsResponse { models, meta }),
        Err(source) => Err(invalid_response(meta, endpoint, source)),
    }
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
