// import hell
// ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^
use futures::stream::StreamExt;
use lazy_static::lazy_static;
use std::str::FromStr;
use std::time::Duration;
use std::{env, error::Error, sync::Arc};
use twilight_cache_inmemory::{InMemoryCache, ResourceType};
use twilight_gateway::{
    cluster::{Cluster, ShardScheme},
    Event, Intents,
};
use twilight_http::request::channel::reaction::RequestReactionType;
use twilight_http::Client as HttpClient;
use twilight_model::channel::{embed::Embed, Message};
use twilight_model::id::marker::*;
use twilight_model::id::Id as TwilightId;
use twilight_standby::Standby;

type RoleId = TwilightId<RoleMarker>;
type EmojiId = TwilightId<EmojiMarker>;

static USAGE: &str = include_str!("../help.md");

lazy_static! {
    static ref DB: sled::Db = sled::open(env::var("SLED_PATH").unwrap()).unwrap();
    static ref MOD_ROLE_IDS: Vec<RoleId> = env::var("MOD_ROLE_IDS")
        .expect("please specify ids for roles with permission to execute management commands")
        .split(',')
        .map(|v| v.parse::<RoleId>().expect("invalid role id"))
        .collect();
    static ref REQWEST_CLIENT: reqwest::Client = reqwest::Client::new();
}

macro_rules! log_err {
    ($e:expr) => {
        match $e {
            Ok(_) => (),
            Err(e) => eprintln!("got error! {}", e),
        }
    };
}

// ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^

#[derive(serde::Serialize, serde::Deserialize)]
enum Response {
    React(Vec<String>),
    SimpleMessage(String),
    Embed(Embed),
}

// ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let token = env::var("DISCORD_TOKEN")?;

    let scheme = ShardScheme::Auto;

    let intents = Intents::GUILD_MESSAGES | Intents::DIRECT_MESSAGES;

    let (cluster, mut events) = Cluster::builder(token.clone(), intents)
        .shard_scheme(scheme)
        .build()
        .await?;

    let cluster = Arc::new(cluster);

    let cluster_spawn = cluster.clone();

    tokio::spawn(async move {
        cluster_spawn.up().await;
    });

    let standby = Arc::new(Standby::new());

    let http = Arc::new(HttpClient::new(token));

    let cache = InMemoryCache::builder()
        .resource_types(ResourceType::MESSAGE)
        .build();

    while let Some((shard_id, event)) = events.next().await {
        standby.process(&event);
        cache.update(&event);
        tokio::spawn(handle_event(
            shard_id,
            event,
            Arc::clone(&http),
            Arc::clone(&standby),
        ));
    }

    Ok(())
}

// ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^

async fn handle_event(
    shard_id: u64,
    event: Event,
    http: Arc<HttpClient>,
    standby: Arc<Standby>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match event {
        Event::MessageCreate(msg) => {
            if let Some(response) = DB.get(msg.content.trim().to_lowercase())? {
                log_err!(send_response(msg.0, &response, &http).await);
            } else if let Some(_) = msg
                .member
                .as_ref()
                .filter(|member| member.roles.iter().any(|v| MOD_ROLE_IDS.contains(v)))
            {
                if let Some(command) = msg
                    .0
                    .content
                    .strip_prefix("sakamoto!")
                    .map(|v| v.to_owned())
                {
                    log_err!(handle_command(&command, msg.0, &http, &standby).await);
                }
            }
        }
        Event::ShardConnected(_) => {
            println!("Connected on shard {}", shard_id);
        }
        _ => {}
    }

    Ok(())
}

// // ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^

async fn send_response(
    msg: Message,
    response: &[u8],
    http: &Arc<HttpClient>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let response = serde_json::from_slice(&response[..])?;
    match response {
        Response::React(emojis) => {
            for e in emojis {
                let react = if let Some(id) = e
                    .strip_prefix(":custom:")
                    .and_then(|v| EmojiId::from_str(v).ok())
                {
                    RequestReactionType::Custom { id, name: None }
                } else {
                    RequestReactionType::Unicode { name: &e }
                };

                http.create_reaction(msg.channel_id, msg.id, &react)
                    .exec()
                    .await?;
            }
        }
        Response::SimpleMessage(content) => {
            http.create_message(msg.channel_id)
                .content(&content)?
                .exec()
                .await?;
        }
        Response::Embed(embed) => {
            http.create_message(msg.channel_id)
                .embeds(&[embed])?
                .exec()
                .await?;
        }
    }

    Ok(())
}

// ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^  ^~^

async fn handle_command(
    command: &str,
    msg: Message,
    http: &Arc<HttpClient>,
    standby: &Arc<Standby>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let args: Vec<&str> = command.split_whitespace().collect();
    match args[0] {
        "add_react" => {
            if args.len() < 3 {
                http.create_message(msg.channel_id).content("invalid usage: proper `sakamoto! add_react [content to trigger on] [emojis separated by spaces]`")?.exec().await?;
            } else {
                DB.insert(
                    args[1],
                    serde_json::to_vec(&Response::React(
                        (&args[2..])
                            .iter()
                            .map(|v| v.to_string())
                            .collect::<Vec<String>>(),
                    ))?,
                )?;
                http.create_message(msg.channel_id)
                    .content("created new trigger!")?
                    .exec()
                    .await?;
            }
        }
        "add_message" => {
            if args.len() < 2 {
                http.create_message(msg.channel_id)
                    .content(
                        "invalid usage: proper `sakamoto! add_mesage [content to trigger on]`",
                    )?
                    .exec()
                    .await?;
            } else {
                http.create_message(msg.channel_id).content("ok! now please send a message containing the content you want, bracketed in \\`\\`\\`.")?.exec().await?;
                let author = msg.author.id.clone();
                let response_content = tokio::time::timeout(
                    Duration::from_secs(60 * 10),
                    standby.wait_for(msg.guild_id.unwrap(), move |event: &Event| {
                        if let Event::MessageCreate(ref new_msg) = event {
                            author == new_msg.author.id && new_msg.content.starts_with("```")
                        } else {
                            false
                        }
                    }),
                )
                .await?
                .map(|v| {
                    if let Event::MessageCreate(new_msg) = v {
                        return new_msg;
                    } else {
                        unreachable!()
                    }
                })?;

                DB.insert(
                    args[1],
                    serde_json::to_vec(&Response::SimpleMessage(
                        response_content
                            .0
                            .content
                            .trim_start_matches("```")
                            .trim_end_matches("```")
                            .to_owned(),
                    ))?,
                )?;

                http.create_message(msg.channel_id)
                    .content("created new trigger!")?
                    .exec()
                    .await?;
            }
        }
        "add_embed" => {
            if args.len() < 2 {
                http.create_message(msg.channel_id)
                    .content("invalid usage: proper `sakamoto! add_embed [content to trigger on]`")?
                    .exec()
                    .await?;
            } else {
                http.create_message(msg.channel_id).content("ok! now please send a message containing the json of the embed you want, bracketed in \\`\\`\\` or as an attachment.")?.exec().await?;
                let author = msg.author.id.clone();
                let mut response_content = tokio::time::timeout(
                    Duration::from_secs(60 * 10),
                    standby.wait_for(msg.guild_id.unwrap(), move |event: &Event| {
                        if let Event::MessageCreate(ref new_msg) = event {
                            author == new_msg.author.id
                                && (new_msg.content.starts_with("```")
                                    || !new_msg.attachments.is_empty())
                        } else {
                            false
                        }
                    }),
                )
                .await?
                .map(|v| {
                    if let Event::MessageCreate(new_msg) = v {
                        return new_msg;
                    } else {
                        unreachable!()
                    }
                })?;

                let mut embed: serde_json::Value =
                    if let Some(attach) = response_content.attachments.pop() {
                        REQWEST_CLIENT.get(attach.url).send().await?.json().await?
                    } else {
                        serde_json::from_str(
                            response_content
                                .content
                                .trim_start_matches("```")
                                .trim_end_matches("```"),
                        )?
                    };

                // lol. lmao
                embed["type"] = serde_json::json!("rich");

                let embed = serde_json::from_value(embed)?;

                DB.insert(args[1], serde_json::to_vec(&Response::Embed(embed))?)?;

                http.create_message(msg.channel_id)
                    .content("created new trigger!")?
                    .exec()
                    .await?;
            }
        }
        "list_triggers" => {
            let mut triggers = String::from("**active response tags:**\n");
            for (k, v) in DB
                .iter()
                .collect::<Result<Vec<(sled::IVec, sled::IVec)>, sled::Error>>()?
            {
                let response = serde_json::from_slice(&v)?;

                triggers.push_str(&format!(
                    "-> {} (kind: {})\n",
                    std::str::from_utf8(&k)?,
                    match response {
                        Response::Embed(_) => "embed",
                        Response::SimpleMessage(_) => "simple message",
                        Response::React(_) => "react",
                    }
                ));
            }

            http.create_message(msg.channel_id)
                .content(&triggers)?
                .exec()
                .await?;
        }
        "remove_trigger" => {
            if args.len() < 2 {
                http.create_message(msg.channel_id)
                    .content("invalid usage: proper `sakamoto! remove [content to trigger on]`")?
                    .exec()
                    .await?;
            } else {
                if let Some(_) = DB.remove(args[1])? {
                    http.create_message(msg.channel_id)
                        .content("trigger deleted!")?
                        .exec()
                        .await?;
                } else {
                    http.create_message(msg.channel_id)
                        .content("trigger not found")?
                        .exec()
                        .await?;
                }
            }
        }
        "help" => {
            http.create_message(msg.channel_id)
                .content(USAGE)?
                .exec()
                .await?;
        }
        _ => (),
    }

    Ok(())
}
