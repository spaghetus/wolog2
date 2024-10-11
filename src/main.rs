use article::ArticleMeta;
use article::{error::ArticleError, ArticleManager, ArticlePath};
use chrono::{Date, DateTime, Duration, Local, NaiveDate};
use pandoc_ast::Map;
use rocket::request::{FromParam, FromSegments};
use rocket::serde::json::Json;
use rocket::{fs::FileServer, Rocket};
use rocket::{tokio, State};
use rocket_dyn_templates::{context, Template};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::default;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

mod article;
mod filters;

#[macro_use]
extern crate rocket;

#[rocket::main]
async fn main() {
    Rocket::build()
        .attach(Template::fairing())
        .manage(ArticleManager::default())
        .mount(
            "/",
            routes![
                force_refresh,
                show_article,
                render_homepage,
                search,
                tags,
                tags_list
            ],
        )
        .mount("/assets", FileServer::from("./articles/assets"))
        .mount("/static", FileServer::from("./static"))
        .launch()
        .await
        .expect("Rocket failed");
}

#[get("/force-refresh")]
async fn force_refresh(article_manager: &State<ArticleManager>) -> &'static str {
    article_manager.force_rescan().await;
    "OK"
}

#[get("/")]
async fn render_homepage(
    article_manager: &State<ArticleManager>,
) -> Result<Template, ArticleError> {
    let articles = article_manager
        .get_all_articles(Path::new("./articles"))
        .await?;
    let mut articles: Vec<_> = articles
        .into_iter()
        .filter(|(_, article)| !article.exclude_from_rss)
        .collect();
    articles.sort_by_key(|(_, a)| a.created);
    articles.reverse();
    articles = articles.into_iter().take(9).collect();
    Ok(Template::render("homepage", context! {articles}))
}

#[get("/<article..>")]
async fn show_article(
    article: ArticlePath,
    article_manager: &State<ArticleManager>,
) -> Result<Template, ArticleError> {
    article_manager
        .get_article(&article)
        .await
        .map(|a| a.as_ref().into())
}

#[derive(Serialize, Deserialize, Default, Clone)]
enum SortType {
    CreateAsc,
    #[default]
    CreateDesc,
    UpdateAsc,
    UpdateDesc,
    NameAsc,
    NameDesc,
}

impl SortType {
    pub fn sort_fn(
        &self,
    ) -> &dyn Fn(&(PathBuf, ArticleMeta), &(PathBuf, ArticleMeta)) -> std::cmp::Ordering {
        match self {
            SortType::CreateAsc => &|(_, l), (_, r)| l.created.cmp(&r.created),
            SortType::CreateDesc => &|(_, l), (_, r)| r.created.cmp(&l.created),
            SortType::UpdateAsc => &|(_, l), (_, r)| l.updated.cmp(&r.updated),
            SortType::UpdateDesc => &|(_, l), (_, r)| r.updated.cmp(&l.updated),
            SortType::NameAsc => &|(_, l), (_, r)| l.title.cmp(&r.title),
            SortType::NameDesc => &|(_, l), (_, r)| r.title.cmp(&l.title),
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[get("/search/<search_path..>?<created_since>&<created_before>&<updated_since>&<updated_before>&<tags>&<title_filter>&<sort_type>")]
async fn search(
    search_path: PathBuf,
    tags: Vec<String>,
    created_since: Option<Json<NaiveDate>>,
    created_before: Option<Json<NaiveDate>>,
    updated_since: Option<Json<NaiveDate>>,
    updated_before: Option<Json<NaiveDate>>,
    article_manager: &State<ArticleManager>,
    title_filter: Option<String>,
    sort_type: Option<Json<SortType>>,
) -> Result<Template, ArticleError> {
    let path = Path::new("./articles").join(&search_path);
    let created_range = created_since
        .as_deref()
        .cloned()
        .unwrap_or(NaiveDate::from_ymd_opt(1, 1, 1).unwrap())
        ..=created_before
            .as_deref()
            .cloned()
            .unwrap_or(NaiveDate::from_ymd_opt(9999, 1, 1).unwrap());
    let updated_range = updated_since
        .as_deref()
        .cloned()
        .unwrap_or(NaiveDate::from_ymd_opt(1, 1, 1).unwrap())
        ..=updated_before
            .as_deref()
            .cloned()
            .unwrap_or(NaiveDate::from_ymd_opt(9999, 1, 1).unwrap());
    let title_filter = title_filter.as_deref().unwrap_or("");
    let sort_type = sort_type.as_deref().cloned().unwrap_or_default();
    let articles = article_manager.get_all_articles(&path).await?;
    let mut articles = articles
        .into_iter()
        .filter(|(p, meta)| {
            created_range.contains(&meta.created)
                && updated_range.contains(&meta.updated)
                && tags.iter().all(|tag| meta.tags.contains(tag))
                && meta.title.contains(title_filter)
        })
        .collect::<Vec<_>>();
    articles.sort_by(sort_type.sort_fn());
    Ok(Template::render(
        "page-list",
        context! {
            search_path,
            sort_type,
            title_filter,
            tags,
            created_since: created_range.start(),
            created_before: created_range.end(),
            updated_since: updated_range.start(),
            updated_before: updated_range.end(),
            articles
        },
    ))
}

#[get("/tags/list")]
async fn tags_list(article_manager: &State<ArticleManager>) -> Result<Template, ArticleError> {
    let articles = article_manager
        .get_all_articles(Path::new("./articles"))
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
#[get("/tags/<search_path..>?<tags..>")]
async fn tags(
    search_path: PathBuf,
    tags: Vec<String>,
    article_manager: &State<ArticleManager>,
) -> Result<Template, ArticleError> {
    let path = Path::new("./articles").join(&search_path);
    let articles = article_manager.get_all_articles(&path).await?;
    let mut articles = articles
        .into_iter()
        .filter(|(_p, meta)| tags.iter().any(|tag| meta.tags.contains(tag)))
        .collect::<Vec<_>>();
    articles.sort_by(|(_, l), (_, r)| r.created.cmp(&l.created));
    Ok(Template::render(
        "tag-list",
        context! {
            search_path,
            tags,
            articles
        },
    ))
}
