use article::{error::ArticleError, ArticlePath};
use article::{Article, ArticleMeta, Bounds, Search, SortType};
use atom_syndication::{Category, Content, Entry, Generator, Link, Person, Text};
use chrono::{
    Date, DateTime, Days, Duration, FixedOffset, Local, NaiveDate, NaiveDateTime, NaiveTime,
    TimeZone, Utc,
};
use pandoc_ast::Map;
use rocket::form::{Form, FromFormField, ValueField};
use rocket::http::hyper::Request;
use rocket::http::{ContentType, HeaderMap, Status};
use rocket::request::{FromParam, FromRequest, FromSegments, Outcome};
use rocket::response::Responder;
use rocket::serde::json::Json;
use rocket::tokio::runtime::{Handle, Runtime};
use rocket::{fs::FileServer, Rocket};
use rocket::{tokio, State};
use rocket_dyn_templates::{context, Template};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::default;
use std::ops::{Bound, Deref, RangeBounds};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, LazyLock, RwLock};

mod article;
mod db;
mod filters;

static WOLOG_URL: LazyLock<String> = LazyLock::new(|| {
    dbg!(std::env::var("WOLOG_URL").unwrap_or_else(|_| "https://wolo.dev/".to_string()))
});

#[macro_use]
extern crate rocket;

#[rocket::main]
async fn main() {
    Rocket::build()
        .attach(Template::fairing())
        // .manage(Arc::new(ArticleManager::default()))
        .mount(
            "/",
            routes![
                show_article,
                render_homepage,
                search,
                tags,
                tags_list,
                gen_feed,
                mention
            ],
        )
        .mount("/assets", FileServer::from("./articles/assets"))
        .mount("/static", FileServer::from("./static"))
        .launch()
        .await
        .expect("Rocket failed");
}

#[get("/")]
async fn render_homepage() -> Result<Template, ArticleError> {
    show_article(ArticlePath("articles/index.md".into())).await
}

#[get("/<article..>")]
async fn show_article(article: ArticlePath) -> Result<Template, ArticleError> {
    let article = article::get_article(&article.0.into()).await?;
    Ok((&*article).into())
}

pub struct Feed(pub atom_syndication::Feed);

impl<'r, 'o: 'r> Responder<'r, 'o> for Feed {
    fn respond_to(self, request: &'r rocket::Request<'_>) -> rocket::response::Result<'o> {
        let response = self.0.to_string();
        let mut response = response.respond_to(request)?;
        response.set_header(ContentType::new("application", "atom+xml"));
        Ok(response)
    }
}

pub struct ModifiedSince(pub DateTime<Utc>);

#[async_trait]
impl<'r> FromRequest<'r> for ModifiedSince {
    type Error = &'static str;
    async fn from_request(request: &'r rocket::request::Request<'_>) -> Outcome<Self, Self::Error> {
        let Some(header) = request.headers().get("If-Modified-Since").next() else {
            return Outcome::Error((Status::BadRequest, "No If-Modified-Since"));
        };
        let Ok(time) = DateTime::parse_from_rfc2822(header) else {
            return Outcome::Error((Status::BadRequest, "Bad timestamp"));
        };
        rocket::outcome::Outcome::Success(Self(time.into()))
    }
}

#[get("/feed/<path..>")]
async fn gen_feed(
    path: PathBuf,
    modified_since: Option<ModifiedSince>,
) -> Result<Feed, ArticleError> {
    fn naive_date_to_time(date: NaiveDate) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(0)
            .unwrap()
            .from_local_datetime(&NaiveDateTime::new(date, NaiveTime::default()))
            .unwrap()
    }
    let search = Search {
        created: (
            match modified_since {
                Some(t) => Bound::Included(t.0.date_naive()),
                None => Bound::Unbounded,
            },
            Bound::Unbounded,
        ),
        search_path: path.clone(),
        ..Default::default()
    };
    let mut search = article::search(&search).await?;
    dbg!(search.len());
    search.retain(|(_, a)| !a.exclude_from_rss);
    let mut rt = Handle::current();
    let search = {
        let mut new = vec![];
        for (path, meta) in search {
            let Ok(article) = article::get_article(&Path::new("articles").join(&path).into()).await
            else {
                continue;
            };
            new.push((path.clone(), article));
        }
        new
    };
    let feed = atom_syndication::Feed {
        title: "Willow's blog".into(),
        id: format!("https://wolo.dev/{}", path.to_string_lossy()),
        base: Some("https://wolo.dev/".to_string()),
        updated: naive_date_to_time(
            search
                .iter()
                .map(|(_, a)| a.meta.updated)
                .max()
                .unwrap_or_default(),
        ),
        authors: vec![Person {
            name: "Willow".into(),
            email: Some("public@w.wolo.dev".into()),
            uri: Some("https://wolo.dev".into()),
        }],
        categories: search
            .iter()
            .flat_map(|(_, a)| a.meta.tags.as_slice())
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|t| Category {
                term: t.to_string(),
                ..Default::default()
            })
            .collect(),
        generator: Some(Generator {
            value: "Wolog".into(),
            ..Default::default()
        }),
        links: vec![Link {
            href: "https://wolo.dev".to_string(),
            rel: "alternate".to_string(),
            mime_type: Some("text/html".to_string()),
            ..Default::default()
        }],
        rights: Some("https://creativecommons.org/licenses/by-nc/4.0/".into()),
        entries: search
            .iter()
            .map(|(p, a)| Entry {
                title: a.meta.title.clone().into(),
                id: p.to_string_lossy().to_string(),
                updated: naive_date_to_time(a.meta.updated),
                categories: a
                    .meta
                    .tags
                    .as_slice()
                    .iter()
                    .map(|t| Category {
                        term: t.to_string(),
                        ..Default::default()
                    })
                    .collect(),
                contributors: vec![],
                links: vec![Link {
                    href: format!("https://wolo.dev/{}", p.to_string_lossy()),
                    rel: "alternate".to_string(),
                    mime_type: Some("text/html".to_string()),
                    ..Default::default()
                }],
                published: Some(naive_date_to_time(a.meta.created)),
                summary: Some(Text {
                    base: Some(format!("https://wolo.dev/{}", p.to_string_lossy())),
                    value: a.content.clone(),
                    r#type: atom_syndication::TextType::Html,
                    ..Default::default()
                }),
                content: Some(Content {
                    base: Some(format!("https://wolo.dev/{}", p.to_string_lossy())),
                    value: Some(a.content.clone()),
                    src: Some(format!("https://wolo.dev/{}", p.to_string_lossy())),
                    content_type: Some("text/html".into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    Ok(Feed(feed))
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(transparent)]
pub struct DateField(pub NaiveDate);

impl Deref for DateField {
    type Target = NaiveDate;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'r> FromFormField<'r> for DateField {
    fn from_value(field: ValueField<'r>) -> rocket::form::Result<'r, Self> {
        use rocket::form::error::*;
        let content = field.value;
        if content.is_empty() {
            return Err(Errors::from(ErrorKind::Missing));
        }
        NaiveDate::from_str(content)
            .map(Self)
            .map_err(|e| Errors::from(ErrorKind::Validation(e.to_string().into())))
    }
}

#[allow(clippy::too_many_arguments)]
#[get("/search/<search_path..>?<created_since>&<created_before>&<updated_since>&<updated_before>&<tags>&<title_filter>&<sort_type>")]
async fn search(
    search_path: PathBuf,
    tags: Vec<String>,
    created_since: Option<DateField>,
    created_before: Option<DateField>,
    updated_since: Option<DateField>,
    updated_before: Option<DateField>,
    title_filter: Option<String>,
    sort_type: Option<SortType>,
) -> Result<Template, ArticleError> {
    let created = (
        created_since
            .as_deref()
            .cloned()
            .map(Bound::Included)
            .unwrap_or(Bound::Unbounded),
        created_before
            .as_deref()
            .cloned()
            .map(Bound::Included)
            .unwrap_or(Bound::Unbounded),
    );
    let updated = (
        updated_since
            .as_deref()
            .cloned()
            .map(Bound::Included)
            .unwrap_or(Bound::Unbounded),
        updated_before
            .as_deref()
            .cloned()
            .map(Bound::Included)
            .unwrap_or(Bound::Unbounded),
    );
    let sort_type = sort_type.unwrap_or_default();
    let search = Search {
        search_path: search_path.clone(),
        title_filter: title_filter.clone(),
        tags: tags.clone(),
        sort_type,
        created,
        updated,
        ..Default::default()
    };
    let articles = article::search(&search).await?;
    Ok(Template::render(
        "page-list",
        context! {
            search_path,
            sort_type,
            title_filter,
            tags,
            created_since,
            created_before,
            updated_since,
            updated_before,
            articles
        },
    ))
}

#[get("/tags/list")]
async fn tags_list() -> Result<Template, ArticleError> {
    let articles = article::search(&Search::default()).await?;
    let tags: BTreeMap<&str, usize> = articles
        .iter()
        .flat_map(|(_, meta)| meta.tags.iter().map(|s| s.as_str()))
        .fold(BTreeMap::new(), |mut acc, el| {
            *acc.entry(el).or_insert(0) += 1;
            acc
        });
    Ok(Template::render(
        "tag-directory",
        context! {
            tags
        },
    ))
}

#[get("/tags/<search_path..>?<sort_type>&<tags..>")]
async fn tags(
    search_path: PathBuf,
    tags: Vec<String>,
    sort_type: Option<SortType>,
) -> Result<Template, ArticleError> {
    let sort_type = sort_type.unwrap_or_default();
    let articles = article::search(&Search {
        search_path: search_path.clone(),
        tags: tags.clone(),
        sort_type,
        ..Default::default()
    })
    .await?;
    Ok(Template::render(
        "tag-list",
        context! {
            search_path,
            tags,
            articles
        },
    ))
}

#[derive(FromForm)]
struct WebMention {
    pub source: String,
    pub target: String,
}

#[post("/webmention", data = "<webmention>")]
async fn mention(webmention: Form<WebMention>) -> Status {
    let Some(target) = webmention.target.strip_prefix(&*WOLOG_URL) else {
        return Status::BadRequest;
    };
    let target = target.trim_start_matches("/");
    tokio::spawn(db::received_webmention(
        webmention.source.clone(),
        target.to_string(),
    ));
    Status::Accepted
}
