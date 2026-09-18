//! The models endpoint.
//!
//! Listing the models is also how a client checks that its API key works and
//! opens its connection before the first real call: the request carries no
//! body, so a key that is rejected is rejected cheaply.
