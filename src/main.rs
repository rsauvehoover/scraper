use std::path::Path;

mod config;
mod db;
mod epub;
mod mail;
mod scraper;

#[tokio::main]
async fn main() {
    let config = config::load_config();

    let conn = match db::open() {
        Ok(conn) => conn,
        Err(e) => {
            panic!("Error opening database: {}", e);
        }
    };

    let client = match scraper::build_client().await {
        Ok(client) => client,
        Err(e) => panic!("Error building request client: {}", e),
    };

    match scraper::update_index(&conn, &config.toc_url, &client).await {
        Ok(_) => (),
        Err(e) => panic!("Error updating index: {}", e),
    }

    match scraper::download_all_chapters(
        &conn,
        &config.request_delay,
        &config.patreon_name,
        &client,
    )
    .await
    {
        Ok(_) => (),
        Err(e) => panic!("Error getting chapters: {}", e),
    }

    match epub::generate_epubs(&conn, Path::new("build/"), &config).await {
        Ok(_) => (),
        Err(e) => panic!("Error generating epubs: {}", e),
    }
}
