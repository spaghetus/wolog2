use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};

use reqwest::Client;
use rocket::tokio::{
    runtime::Handle,
    sync::{OnceCell, Semaphore},
};
use sqlx::{migrate, Pool, Sqlite};

use crate::WOLOG_URL;

static DB: OnceCell<Pool<Sqlite>> = OnceCell::const_new();

async fn db() -> &'static Pool<Sqlite> {
    DB.get_or_init(|| async {
        if let Some(db) = connect_to_disk().await {
            db
        } else {
            connect_to_memory().await
        }
    })
    .await
}

async fn connect_to_disk() -> Option<Pool<Sqlite>> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = Pool::connect_lazy(&url).ok()?;
    println!("Start running migrations...");
    migrate!().run(&pool).await.expect("Migrations failed");
    println!("Done running migrations!");
    Some(pool)
}

async fn connect_to_memory() -> Pool<Sqlite> {
    let pool = Pool::connect_lazy("sqlite::memory:").unwrap();
    println!("Start running migrations...");
    migrate!().run(&pool).await.expect("Migrations failed");
    println!("Done running migrations!");
    pool
}

static WEBMENTION_BUCKET: LazyLock<Arc<Semaphore>> = LazyLock::new(|| {
    let semaphore = Arc::new(Semaphore::new(8));
    Handle::current().spawn({
        let semaphore = semaphore.clone();
        async move {
            let mut clock = rocket::tokio::time::interval(Duration::from_secs(1));
            loop {
                if semaphore.available_permits() < 8 {
                    semaphore.add_permits(1);
                }
                clock.tick().await;
            }
        }
    });
    semaphore
});

pub async fn received_webmention(from: String, to: String) {
    WEBMENTION_BUCKET.acquire().await.unwrap().forget();
    let Ok(mut mentioner) = reqwest::get(&from).await else {
        return;
    };
    let Ok(Some(mentioner)): Result<_, reqwest::Error> = async {
        let mut body = vec![];
        while let Some(chunk) = mentioner.chunk().await? {
            body.extend(chunk);
            if body.len() > 0xFFFFFF {
                return Ok(None);
            }
        }
        Ok(Some(body))
    }
    .await
    else {
        return;
    };
    let Ok(mentioner) = String::from_utf8(mentioner) else {
        return;
    };
    let expected_url = WOLOG_URL.to_string() + &to.replace(" ", "%20");
    if !mentioner.contains(&expected_url) {
        return;
    }
    if let Err(e) = sqlx::query!(
        "INSERT OR REPLACE INTO received_mentions VALUES($1, $2)",
        from,
        to
    )
    .execute(db().await)
    .await
    {
        eprintln!("Error writing webmention: {e}");
    }
}

pub async fn mentions_of(article: &str) -> Vec<String> {
    let data: Vec<_> = sqlx::query!(
        "SELECT from_url FROM received_mentions WHERE to_path = $1",
        article
    )
    .fetch_all(db().await)
    .await
    .unwrap_or_default();
    data.into_iter().map(|v| v.from_url).collect()
}

pub async fn send_webmention(from: String, to: String) {}
