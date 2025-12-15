use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct MailConfig {
    pub name: String,
    pub address: String,
    pub password: String,
    pub smtp_hostname: String,
    pub smtp_port: u16,
    pub destinations: Vec<UserConfig>,
}
impl Default for MailConfig {
    fn default() -> Self {
        MailConfig {
            name: String::default(),
            address: String::default(),
            password: String::default(),
            smtp_hostname: String::from("smtp.gmail.com"),
            smtp_port: 587,
            destinations: Vec::<UserConfig>::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct UserConfig {
    pub name: String,
    pub email: String,
    pub strip_colour: bool,
    pub send_full_volumes: bool,
    pub send_individual_chapters: bool,
}
impl Default for UserConfig {
    fn default() -> Self {
        UserConfig {
            name: String::default(),
            email: String::default(),
            strip_colour: false,
            send_full_volumes: true,
            send_individual_chapters: false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct EpubGenConfig {
    pub volumes: bool,
    pub chapters: bool,
    pub strip_colour: bool,
}

impl Default for EpubGenConfig {
    fn default() -> Self {
        EpubGenConfig {
            volumes: true,
            chapters: true,
            strip_colour: false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Config {
    pub mail: MailConfig,
    pub epub_gen: EpubGenConfig,
    pub toc_url: String,
    // number of seconds to wait before allowing another request to be made
    // avoids being ip banned
    pub request_delay: u64,
    pub patreon_name: String,
}
impl Default for Config {
    fn default() -> Self {
        Config {
            toc_url: String::from("https://wanderinginn.com/table-of-contents/"),
            request_delay: 1000,
            mail: MailConfig::default(),
            epub_gen: EpubGenConfig::default(),
            patreon_name: String::default(),
        }
    }
}

pub fn load_config() -> Config {
    if !std::path::Path::new("config.json").exists() {
        println!("No config.json found, using default values");
        println!("Request delay is 1000ms");
        return Config::default();
    }
    match std::fs::read_to_string("config.json") {
        Ok(str) => match serde_json::from_str::<Config>(&str) {
            Ok(mut config) => {
                println!("Loaded config");
                println!("Delay is {}ms", config.request_delay);
                println!(
                    "Sending from <{}> at <{}>",
                    config.mail.name, config.mail.address
                );
                for dest in &config.mail.destinations {
                    println!("Sending to <{}> at <{}>", dest.name, dest.email);
                }

                for dest in &config.mail.destinations {
                    if dest.strip_colour {
                        config.epub_gen.strip_colour = true;
                    }
                    if dest.send_full_volumes {
                        config.epub_gen.volumes = true;
                    }
                    if dest.send_individual_chapters {
                        config.epub_gen.chapters = true;
                    }
                }

                config
            }
            Err(e) => {
                panic!("{}", e);
            }
        },
        Err(e) => {
            panic!("{}", e);
        }
    }
}
