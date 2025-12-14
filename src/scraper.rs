use regex::Regex;
use reqwest::{header::HeaderValue, header::COOKIE, header::USER_AGENT, Client, Response};
use rusqlite::{Connection, Result};
use soup::prelude::*;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db;

use std::{thread, time::Duration};

pub async fn build_client() -> Result<Client, Box<dyn std::error::Error>> {
    let client = Client::builder().cookie_store(true).build()?;

    Ok(client)
}

async fn get_html(
    uri: String,
    patreon_name: &String,
    client: &Client,
) -> Result<String, Box<dyn std::error::Error>> {
    let resp: Response;

    if patreon_name.is_empty() {
        resp = client.get(uri).header(USER_AGENT, "reqwest").send().await?;
    } else {
        let epoch_stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();

        let cookie = format!("patreon_verified=1; patreon_verified_time={epoch_stamp}; patreon_patron_status=active_patron; patreon_user_name={patreon_name};");
        resp = client
            .get(uri)
            .header(USER_AGENT, "reqwest")
            .header(COOKIE, HeaderValue::from_str(&cookie).unwrap())
            .send()
            .await?;
    }

    let body = resp.text().await?;
    Ok(body)
}

pub async fn update_index(
    db_conn: &Connection,
    toc_url: &String,
    client: &Client,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("(Re)Building index");

    let soup = Soup::new(&get_html(toc_url.to_string(), &String::default(), client).await?);

    for volume in soup.class("volume-wrapper").find_all() {
        let volume_title = volume.tag("h2").find().unwrap().text();
        let volume_id: usize = db::add_volume(db_conn, &volume_title)?;
        let mut count = 0;
        for chapter in volume.class("chapter-entry").find_all() {
            let a = chapter.tag("a").find().unwrap();
            let uri = a.get("href").unwrap();
            let title = a.text().trim().to_string();
            db::add_chapter(db_conn, title, uri, volume_id)?;
            count += 1;
        }
        println!("Indexed {volume_title} with {count} chapters");
    }

    println!("Finished building index");

    Ok(())
}

async fn download_chapter(
    db_conn: &Connection,
    chapter: db::Chapter,
    patreon_name: &String,
    client: &Client,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut html_string = get_html(chapter.uri, patreon_name, client).await?;

    let escape_re = Regex::new(r"(?:&)((?:lt|gt|nbsp);)").unwrap();
    html_string = escape_re
        .replace_all(&html_string, |captures: &regex::Captures| {
            format!("&amp;{}", &captures[1])
        })
        .to_string();

    let soup = Soup::new(&html_string);
    let html = soup.attr("id", "main-content").find().unwrap();

    let header = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.1//EN\" \"http://www.w3.org/TR/xhtml11    /DTD/xhtml11.dtd\">
<html xmlns=\"http://www.w3.org/1999/xhtml\">
<head>
<meta http-equiv=\"Content-Type\" content=\"text/html; charset=UTF-8\" />
<meta name=\"author\" content=\"pirate aba\"/>
<meta name=\"description\" content=\"The Wandering Inn\"/>
<meta name=\"classification\" content=\"Fantasy\" />
<title>The Wandering Inn</title>
<link rel=\"stylesheet\" href=\"style.css\" type = \"text/css\" />
</head>
<body>";

    let title = format!("<h1>{}</h1>", soup.tag("title").find().unwrap().text());

    let patron_re = Regex::new(r"(?i)Patron Early Access").unwrap();
    let is_patreon_chapter = patron_re.is_match(&title);

    let re = Regex::new(r"<a.*?</a>").unwrap();
    let body = html.display();
    let footer = "</body></html>";

    if is_patreon_chapter && patreon_name.is_empty() {
        db::remove_chapter(db_conn, chapter.id)?;
    } else {
        db::add_chapter_data(
            db_conn,
            chapter.id,
            &format!(
                "{}\n{}\n{}\n{}\n",
                header,
                title,
                re.replace_all(&body, ""),
                footer
            ),
        )?;
    }
    Ok(())
}

pub async fn download_all_chapters(
    db_conn: &Connection,
    delay: &u64,
    patreon_name: &String,
    client: &Client,
) -> Result<(), Box<dyn std::error::Error>> {
    let chapters = db::get_empty_chapters(db_conn)?;

    if chapters.is_empty() {
        println!("No chapters to download");
    } else {
        println!("Downloading {} missing chapters", chapters.len());
    }
    let mut count = 0;
    for chapter in chapters {
        if count % 10 == 0 && count != 0 {
            println!("Downloaded {} chapters", count);
        }
        thread::sleep(Duration::from_millis(*delay));
        download_chapter(db_conn, chapter, patreon_name, client).await?;
        count += 1;
    }
    println!(
        "{}",
        if count > 0 {
            format!("Done downloading {count} chapters")
        } else {
            "No chapters to download".to_string()
        }
    );
    Ok(())
}
