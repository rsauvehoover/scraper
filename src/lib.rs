//! Multi-source web serial scraper.
//!
//! The `wandering_inn_scraper` binary is a CLI over these modules; the
//! `wandering_inn_scraper web` serves them over HTTP.

pub mod config;
pub mod db;
pub mod epub;
pub mod error;
pub mod mail;
pub mod postprocess;
pub mod sources;
pub mod stats;
pub mod web;
pub mod webconfig;
