use async_recursion::async_recursion;
use chrono::{DateTime, Local, NaiveDate};
use dashmap::{DashMap, DashSet};
use error::ArticleError;
use pandoc_ast::{Block, Inline, MetaValue, Pandoc};
use rocket::{
    form::{FromFormField, ValueField},
    http::uri::Segments,
    request::FromSegments,
    tokio::{self, sync::Mutex},
};
use rocket_dyn_templates::{context, Template};
use serde::{Deserialize, Serialize};
use serde_yml::Value;
use std::{
    ffi::OsStr,
    fmt::Display,
    io::Write,
    ops::{Bound, Deref, RangeBounds},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    str::FromStr,
    sync::{Arc, LazyLock},
    time::{Duration, Instant, SystemTime},
};
use strum::EnumString;

use crate::filters::apply_filters;

pub mod error;

static LAST_REAL_SEARCH: LazyLock<tokio::sync::Mutex<Instant>> =
    LazyLock::new(|| Mutex::new(Instant::now() - Duration::from_secs(3600)));

#[async_recursion]
async fn find_articles(
    path: Arc<Path>,
) -> Result<Vec<(Arc<Path>, Arc<ArticleMeta>)>, ArticleError> {
    if path.is_file() && path.extension() == Some(OsStr::new("md")) {
        if let Ok((meta, _)) = get_metadata(&path).await {
            return Ok(vec![(path.clone(), meta)]);
        }
    }
    if !path.is_dir() {
        return Ok(vec![]);
    }
    let mut dir = tokio::fs::read_dir(path).await?;
    let mut out = vec![];
    while let Some(child) = dir.next_entry().await? {
        let Ok(mut child) = find_articles(child.path().into()).await else {
            continue;
        };
        out.append(&mut child)
    }
    Ok(out)
}

pub async fn search(search: &Search) -> Result<Vec<(Arc<Path>, Arc<ArticleMeta>)>, ArticleError> {
    let mut search_time = LAST_REAL_SEARCH.lock().await;
    let mut articles = if search_time.elapsed() > Duration::from_secs(1800) {
        println!("Do full search");
        *search_time = Instant::now();
        std::mem::drop(search_time);
        find_articles(Path::new("articles").into()).await?
    } else {
        std::mem::drop(search_time);
        AST_CACHE
            .iter()
            .map(|kv| (kv.key().clone(), kv.value().0.clone()))
            .collect()
    };
    articles.retain(|(_, article)| {
        search.created.contains(&article.created)
            && search.updated.contains(&article.updated)
            && !article.hidden
            && search.tags.iter().all(|t| article.tags.contains(t))
            && article
                .title
                .contains(search.title_filter.as_deref().unwrap_or(""))
    });
    let sort = search.sort_type.sort_fn();
    articles.sort_by(|a, b| (sort)(&(&*a.0, &*a.1), &(&*b.0, &*b.1)));
    articles = articles
        .into_iter()
        .map(|(p, a)| (p.strip_prefix("articles").unwrap_or(&p).into(), a))
        .collect();
    Ok(articles)
}

pub async fn get_article(path: &Arc<Path>) -> Result<Arc<Article>, ArticleError> {
    let disk_modified_time = tokio::fs::metadata(&path)
        .await
        .and_then(|m| m.modified())
        .ok();
    let cached = ARTICLE_CACHE.get(path).map(|c| c.value().clone());
    match (disk_modified_time, cached) {
        (None, _) => Err(ArticleError::NoArticle),
        (Some(disk_modified_time), Some(cached))
            if cached.rendered_at >= disk_modified_time && !cached.meta.always_rerender =>
        {
            Ok(cached.clone())
        }
        (Some(_), cached) => match render_article(path).await {
            Ok(article) => Ok(article),
            Err(e) => cached.ok_or(e),
        },
    }
}

async fn render_article(path: &Arc<Path>) -> Result<Arc<Article>, ArticleError> {
    let (meta, ast) = get_metadata(path).await?;

    let ast = ast.to_json();

    let content = rocket::tokio::task::spawn_blocking({
        move || -> Result<_, error::ArticleError> {
            let mut pandoc = Command::new("pandoc")
                .args(["-f", "json", "-t", "html"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?;

            pandoc.stdin.as_mut().unwrap().write_all(ast.as_bytes())?;
            let pandoc = pandoc.wait_with_output()?;

            if !pandoc.status.success() {
                return Err(error::ArticleError::PandocFailed(String::from_utf8(
                    pandoc.stdout,
                )?));
            }

            Ok(String::from_utf8(pandoc.stdout)?)
        }
    })
    .await??;

    let article = Arc::new(Article {
        content,
        meta: meta.clone(),
        rendered_at: SystemTime::now(),
    });
    ARTICLE_CACHE.insert(path.clone(), article.clone());

    Ok(article)
}

async fn get_metadata(path: &Arc<Path>) -> Result<(Arc<ArticleMeta>, Arc<Pandoc>), ArticleError> {
    let disk_modified_time = tokio::fs::metadata(&path)
        .await
        .and_then(|m| m.modified())
        .ok();
    let cached = AST_CACHE.get(path).map(|v| v.clone());
    match (disk_modified_time, cached) {
        (None, _) => Err(ArticleError::NoArticle),
        (Some(disk_modified_time), Some(cached))
            if cached.2 >= disk_modified_time && !cached.0.always_rerender =>
        {
            Ok((cached.0.clone(), cached.1.clone()))
        }
        (Some(_), cached) => match prerender_article(path).await {
            Ok(v) => Ok(v),
            Err(e) => cached.map(|c| (c.0.clone(), c.1.clone())).ok_or(e),
        },
    }
}

async fn prerender_article(
    path: &Arc<Path>,
) -> Result<(Arc<ArticleMeta>, Arc<Pandoc>), ArticleError> {
    if !BUSY_ASTS.insert(path.clone()) {
        println!("Skipping prerendering {path:?} since we're already working on it");
        return AST_CACHE
            .get(path)
            .map(|a| (a.value().0.clone(), a.value().1.clone()))
            .ok_or(ArticleError::NoArticle);
    }
    println!("Rendering {path:?}");
    let ast = tokio::task::spawn_blocking({
        let path = path.clone();
        move || -> Result<_, error::ArticleError> {
            let pandoc = Command::new("pandoc")
                .args(["-f", "markdown", "-t", "json"])
                .arg(path.as_os_str())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .output()?;

            if !pandoc.status.success() {
                return Err(error::ArticleError::PandocFailed(String::from_utf8(
                    pandoc.stdout,
                )?));
            }

            let ast = String::from_utf8(pandoc.stdout)?;
            let ast = Pandoc::from_json(&ast);
            Ok(ast)
        }
    })
    .await??;
    let ast = Arc::new(apply_filters(path.clone(), ast).await);
    let mut meta = ArticleMeta::try_from(&*ast)?;

    let fsmeta = tokio::fs::metadata(path).await.ok();

    let disk_time = fsmeta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .unwrap_or(SystemTime::now());
    let created_time = fsmeta
        .as_ref()
        .and_then(|m| m.created().ok())
        .unwrap_or(SystemTime::now());

    if meta.updated == NaiveDate::default() {
        meta.updated = DateTime::<Local>::from(disk_time).date_naive();
    }
    if meta.created == NaiveDate::default() {
        meta.created = DateTime::<Local>::from(created_time).date_naive();
    }

    let meta = Arc::new(meta);

    AST_CACHE.insert(path.clone(), (meta.clone(), ast.clone(), SystemTime::now()));
    BUSY_ASTS.remove(path);
    Ok((meta, ast))
}

static ARTICLE_CACHE: LazyLock<DashMap<Arc<Path>, Arc<Article>>> = LazyLock::new(DashMap::new);
static AST_CACHE: LazyLock<DashMap<Arc<Path>, (Arc<ArticleMeta>, Arc<Pandoc>, SystemTime)>> =
    LazyLock::new(DashMap::new);
static BUSY_ASTS: LazyLock<DashSet<Arc<Path>>> = LazyLock::new(DashSet::new);

pub type Bounds<B> = (Bound<B>, Bound<B>);

fn unbounded<B>() -> Bounds<B> {
    (Bound::Unbounded, Bound::Unbounded)
}

#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, EnumString)]
pub enum SortType {
    CreateAsc,
    #[default]
    CreateDesc,
    UpdateAsc,
    UpdateDesc,
    NameAsc,
    NameDesc,
}

impl<'r> FromFormField<'r> for SortType {
    fn from_value(field: ValueField<'r>) -> rocket::form::Result<'r, Self> {
        use rocket::form::error::*;
        let content = field.value;
        if content.is_empty() {
            return Err(Errors::from(ErrorKind::Missing));
        }
        Self::from_str(content)
            .map_err(|e| Errors::from(ErrorKind::Validation(e.to_string().into())))
    }
}

impl SortType {
    pub fn sort_fn(
        &self,
    ) -> &dyn Fn(&(&Path, &ArticleMeta), &(&Path, &ArticleMeta)) -> std::cmp::Ordering {
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

#[derive(Serialize, Deserialize, Debug)]
pub struct Search {
    #[serde(default)]
    pub search_path: PathBuf,
    #[serde(default)]
    pub exclude_paths: Vec<PathBuf>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "unbounded")]
    pub created: Bounds<NaiveDate>,
    #[serde(default = "unbounded")]
    pub updated: Bounds<NaiveDate>,
    #[serde(default)]
    pub title_filter: Option<String>,
    #[serde(default)]
    pub sort_type: SortType,
    #[serde(default)]
    pub limit: Option<usize>,
}

impl Default for Search {
    fn default() -> Self {
        Self {
            search_path: Default::default(),
            tags: Default::default(),
            created: (Bound::Unbounded, Bound::Unbounded),
            updated: (Bound::Unbounded, Bound::Unbounded),
            title_filter: Default::default(),
            sort_type: Default::default(),
            exclude_paths: vec![],
            limit: None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Article {
    pub content: String,
    pub meta: Arc<ArticleMeta>,
    pub rendered_at: SystemTime,
}

impl Default for Article {
    fn default() -> Self {
        Self {
            content: Default::default(),
            meta: Default::default(),
            rendered_at: SystemTime::now(),
        }
    }
}

const DEFAULT_TITLE: &dyn Fn() -> String = &|| "Untitled Page".to_string();
const DEFAULT_TEMPLATE: &dyn Fn() -> String = &|| "article".to_string();

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ArticleMeta {
    #[serde(default = "DEFAULT_TITLE")]
    pub title: String,
    #[serde(default)]
    pub blurb: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "DEFAULT_TEMPLATE")]
    pub template: String,
    #[serde(default)]
    pub toc: Vec<Toc>,
    #[serde(default)]
    pub exclude_from_rss: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub updated: NaiveDate,
    #[serde(default)]
    pub created: NaiveDate,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub always_rerender: bool,
    #[serde(flatten)]
    pub extra: Value,
}

impl<'a> TryFrom<&Pandoc> for ArticleMeta {
    type Error = ArticleError;

    fn try_from(pandoc_ast: &Pandoc) -> Result<Self, Self::Error> {
        fn pandoc_inline_to_string(i: &Inline) -> &str {
            match i {
                pandoc_ast::Inline::Str(s) => s.as_str(),
                pandoc_ast::Inline::Space => " ",
                pandoc_ast::Inline::SoftBreak => "\n",
                pandoc_ast::Inline::LineBreak => "\n",
                _ => "",
            }
        }
        fn pandoc_block_to_string(b: &Block) -> String {
            match b {
                Block::Para(i) | Block::Plain(i) => i.iter().map(pandoc_inline_to_string).collect(),
                Block::LineBlock(l) => l
                    .iter()
                    .map(|l| l.iter().map(pandoc_inline_to_string).collect::<String>() + "\n")
                    .collect(),
                Block::RawBlock(_, s) => s.clone(),
                Block::BlockQuote(b) => {
                    b.iter().map(|b| pandoc_block_to_string(b) + "\n").collect()
                }
                _ => String::new(),
            }
        }
        fn pandoc_meta_to_value(meta: MetaValue) -> serde_json::Value {
            use serde_json::Value;
            match meta {
                MetaValue::MetaMap(map) => Value::Object(
                    map.into_iter()
                        .map(|(key, value)| (key, pandoc_meta_to_value(*value)))
                        .collect(),
                ),
                MetaValue::MetaList(list) => {
                    Value::Array(list.into_iter().map(pandoc_meta_to_value).collect())
                }
                MetaValue::MetaBool(b) => Value::Bool(b),
                MetaValue::MetaString(s) => Value::String(s),
                MetaValue::MetaInlines(i) => {
                    Value::String(i.iter().map(pandoc_inline_to_string).collect())
                }
                MetaValue::MetaBlocks(b) => {
                    Value::String(b.iter().map(pandoc_block_to_string).collect())
                }
            }
        }
        let meta = pandoc_ast
            .meta
            .iter()
            .map(|(key, value)| (key.to_string(), pandoc_meta_to_value(value.clone())))
            .collect();
        let meta = serde_json::Value::Object(meta);
        let meta: ArticleMeta = serde_json::from_value(meta)?;
        Ok(meta)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum Toc {
    Text(String),
    Heading {
        label: String,
        anchor: String,
        subheadings: Vec<Toc>,
    },
}

impl Display for Toc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Toc::Text(text) => write!(f, "<li>{text}</li>"),
            Toc::Heading {
                label,
                anchor,
                subheadings,
            } if subheadings.is_empty() => {
                write!(f, "<li><a href=\"#{anchor}\">{label}</a></li>")
            }
            Toc::Heading {
                label,
                anchor,
                subheadings,
            } => write!(
                f,
                "<li><a href=\"#{anchor}\">{label}</a><ul>{}</ul></li>",
                subheadings
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<String>()
            ),
        }
    }
}

pub struct ArticlePath(pub PathBuf);

impl Deref for ArticlePath {
    type Target = std::path::Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'r> FromSegments<'r> for ArticlePath {
    type Error = error::ArticleError;

    fn from_segments(
        segments: Segments<'r, rocket::http::uri::fmt::Path>,
    ) -> Result<Self, Self::Error> {
        let path = segments
            .to_path_buf(false)
            .map_err(error::ArticleError::MalformedPath)?;
        let mut path = Path::new("articles").join(path);
        path.set_extension("md");
        if !path.exists() {
            return Err(error::ArticleError::NotMarkdown);
        }
        Ok(Self(path))
    }
}

impl From<&Article> for Template {
    fn from(article: &Article) -> Template {
        Template::render(
            article.meta.template.clone(),
            context! {
                toc: article.meta.toc.iter().map(ToString::to_string).collect::<String>(),
                meta: &article.meta,
                content: &article.content,
            },
        )
    }
}
