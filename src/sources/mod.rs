mod client;
pub mod content_images;
pub mod manga;
mod generic;
mod registry;
mod royal_road;
pub mod tcb;
pub mod wordpress_comic;
mod traits;
mod wandering_inn;

pub use client::ScraperClient;
pub use registry::ScraperRegistry;
