use article::{error::ArticleError, ArticleManager, ArticlePath};
use article::{ArticleMeta, Bounds, Search, SortType};
use atom_syndication::{Category, Content, Entry, Generator, Link, Person, Text};
use chrono::{
    Date, DateTime, Days, Duration, FixedOffset, Local, NaiveDate, NaiveDateTime, NaiveTime,
    TimeZone, Utc,
};
use pandoc_ast::Map;
use rocket::form::{FromFormField, ValueField};
use rocket::http::hyper::Request;
use rocket::http::{ContentType, HeaderMap, Status};
use rocket::request::{FromParam, FromRequest, FromSegments, Outcome};
use rocket::response::Responder;
use rocket::serde::json::Json;
use rocket::{fs::FileServer, Rocket};
use rocket::{tokio, State};
use rocket_dyn_templates::{context, Template};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::default;
use std::ops::{Bound, Deref, RangeBounds};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, RwLock};

mod article;
mod filters;

#[macro_use]
extern crate rocket;

#[rocket::main]
async fn main() {
    Rocket::build()
        .attach(Template::fairing())
        .manage(Arc::new(ArticleManager::default()))
        .mount(
            "/",
            routes![
                force_refresh,
                show_article,
                render_homepage,
                search,
                tags,
                tags_list,
                gen_feed
            ],
        )
        .mount("/assets", FileServer::from("./articles/assets"))
        .mount("/static", FileServer::from("./static"))
        .launch()
        .await
        .expect("Rocket failed");
}

#[get("/force-refresh")]
async fn force_refresh(article_manager: &State<Arc<ArticleManager>>) -> &'static str {
    article_manager.deref().clone().force_rescan().await;
    "OK"
}

#[get("/")]
async fn render_homepage(
    article_manager: &State<Arc<ArticleManager>>,
) -> Result<Template, ArticleError> {
    let articles = article_manager
        .deref()
        .clone()
        .get_all_articles(Path::new("./articles"), &[])
        .await?;
    let mut articles: Vec<_> = articles
        .into_iter()
        .filter(|(_, article)| !article.exclude_from_rss)
        .collect();
    articles.sort_by_key(|(_, a)| a.created);
    articles.reverse();
    articles = articles.into_iter().take(9).collect();
    let homepage = article_manager
        .deref()
        .clone()
        .get_article(Path::new("articles/index.md"))
        .await
        .ok();
    let homepage = homepage
        .as_deref()
        .map(|a| a.content.as_str())
        .unwrap_or("Create an article called index.md to populate the homepage");
    Ok(Template::render(
        "homepage",
        context! {articles, content: homepage},
    ))
}

#[get("/<article..>")]
async fn show_article(
    article: ArticlePath,
    article_manager: &State<Arc<ArticleManager>>,
) -> Result<Template, ArticleError> {
    article_manager
        .deref()
        .clone()
        .get_article(&article)
        .await
        .map(|a| a.as_ref().into())
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
    article_manager: &State<Arc<ArticleManager>>,
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
    let mut search = article_manager.deref().clone().search(&search).await?;
    dbg!(search.len());
    search.retain(|(_, a)| !a.exclude_from_rss);
    let feed = atom_syndication::Feed {
        title: "Willow's blog".into(),
        id: format!("https://wolo.dev/{}", path.to_string_lossy()),
        base: Some("https://wolo.dev/".to_string()),
        updated: naive_date_to_time(
            search
                .iter()
                .map(|(_, a)| a.updated)
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
            .flat_map(|(_, a)| a.tags.as_slice())
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
            .map(|(p, _)| (p, article_manager.articles.get(p).unwrap().0.clone()))
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
    article_manager: &State<Arc<ArticleManager>>,
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
    let articles = article_manager
        .deref()
        .clone()
        .search(&Search {
            search_path: search_path.clone(),
            tags: tags.clone(),
            created,
            updated,
            title_filter: title_filter.clone(),
            sort_type,
            exclude_paths: vec![],
        })
        .await?;
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
async fn tags_list(article_manager: &State<Arc<ArticleManager>>) -> Result<Template, ArticleError> {
    let articles = article_manager
        .deref()
        .clone()
        .get_all_articles(Path::new("./articles"), &[])
        .await?;
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

#[allow(clippy::too_many_arguments)]
#[get("/tags/<search_path..>?<sort_type>&<tags..>")]
async fn tags(
    search_path: PathBuf,
    tags: Vec<String>,
    article_manager: &State<Arc<ArticleManager>>,
    sort_type: Option<SortType>,
) -> Result<Template, ArticleError> {
    let sort_type = sort_type.unwrap_or_default();
    let articles = article_manager
        .deref()
        .clone()
        .search(&Search {
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
