use async_recursion::async_recursion;
use chrono::{DateTime, Local, NaiveDate};
use dashmap::DashMap;
use error::ArticleError;
use pandoc_ast::{Block, Inline, MetaValue, Pandoc};
use rocket::{
    http::uri::{error::PathError, Segments},
    request::FromSegments,
    response::Responder,
    tokio::{sync::RwLock, task::JoinError},
};
use rocket_dyn_templates::{context, Template};
use serde::{Deserialize, Serialize};
use serde_yml::Value;
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fmt::Display,
    io::{Read, Write},
    ops::Deref,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    str::FromStr,
    string::FromUtf8Error,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use crate::filters;

pub mod error;

pub struct ArticleManager {
    pub articles: DashMap<PathBuf, (Arc<Article>, SystemTime)>,
    pub last_full_scan: RwLock<Instant>,
}

impl Default for ArticleManager {
    fn default() -> Self {
        Self {
            articles: Default::default(),
            last_full_scan: RwLock::new(Instant::now() - Duration::from_secs(36000)),
        }
    }
}

impl ArticleManager {
    pub async fn force_rescan(&self) {
        *self.last_full_scan.write().await = Instant::now() - Duration::from_secs(60 * 60 * 60);
    }
    pub async fn get_article(&self, path: &Path) -> Result<Arc<Article>, error::ArticleError> {
        let existing = self.articles.get(path);
        let meta = rocket::tokio::fs::metadata(&path).await.ok();
        let disk_time = meta.as_ref().and_then(|m| m.modified().ok());
        if let Some(existing) = &existing {
            let cached_time = existing.value().1;
            if disk_time
                .map(|disk_time| disk_time <= cached_time)
                .unwrap_or(true)
            {
                return Ok(existing.value().0.clone());
            }
        }
        let Ok(mut new_article) = Article::render(path, self)
            .await
            .inspect_err(|e| eprintln!("Article {path:?} failed with {e:#?}"))
        else {
            if let Some(existing) = &existing {
                return Ok(existing.value().0.clone());
            } else {
                return Err(error::ArticleError::NoArticle);
            }
        };

        if !new_article.meta.ready {
            if let Some(existing) = &existing {
                return Ok(existing.value().0.clone());
            } else {
                return Err(error::ArticleError::NotForPublication);
            }
        }

        let disk_time = disk_time.unwrap_or(SystemTime::now());
        let created_time = meta
            .as_ref()
            .and_then(|m| m.created().ok())
            .unwrap_or(SystemTime::now());

        if new_article.meta.updated == NaiveDate::default() {
            new_article.meta.updated = DateTime::<Local>::from(disk_time).date_naive();
        }
        if new_article.meta.created == NaiveDate::default() {
            new_article.meta.created = DateTime::<Local>::from(created_time).date_naive();
        }
        std::mem::drop(existing);
        let new_article = Arc::new(new_article);
        self.articles
            .insert(path.to_path_buf(), (new_article.clone(), disk_time));
        Ok(new_article)
    }

    #[async_recursion]
    pub async fn get_all_articles(
        &self,
        path: &Path,
    ) -> Result<HashMap<PathBuf, ArticleMeta>, ArticleError> {
        use rocket::tokio::fs;
        let mut children = fs::read_dir(path).await?;

        let mut out: HashMap<PathBuf, ArticleMeta> = self
            .articles
            .iter()
            .filter(|pair| !pair.value().0.meta.hidden)
            .filter(|pair| pair.key().starts_with(path))
            .map(|pair| (pair.key().clone(), pair.value().0.meta.clone()))
            .collect();
        let md = OsString::from_str("md").unwrap();

        if (Instant::now() - *self.last_full_scan.read().await) > Duration::from_secs(30 * 60) {
            eprintln!("Doing full search");
            while let Some(child) = children.next_entry().await? {
                let path = child.path();
                if child.file_type().await.unwrap().is_dir() {
                    self.get_all_articles(&path)
                        .await
                        .unwrap_or_default()
                        .drain()
                        .filter(|(_, a)| !a.hidden)
                        .for_each(|(key, value)| {
                            out.insert(key, value);
                        })
                } else if path.extension() == Some(&md) {
                    if let Ok(article) = self.get_article(&path).await {
                        out.insert(path, article.meta.clone());
                    };
                }
            }
            if path == Path::new("./articles") {
                *self.last_full_scan.write().await = Instant::now();
            }
        }

        Ok(out)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Article {
    pub content: String,
    pub meta: ArticleMeta,
}

impl Article {
    pub async fn render(
        path: &Path,
        article_mgr: &ArticleManager,
    ) -> Result<Self, error::ArticleError> {
        let pandoc_ast = rocket::tokio::task::spawn_blocking({
            let path = path.to_path_buf();
            move || -> Result<_, error::ArticleError> {
                let pandoc = Command::new("pandoc")
                    .args(["-f", "markdown", "-t", "json"])
                    .arg(path)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .output()?;

                if !pandoc.status.success() {
                    return Err(error::ArticleError::PandocFailed(String::from_utf8(
                        pandoc.stdout,
                    )?));
                }

                Ok(String::from_utf8(pandoc.stdout)?)
            }
        })
        .await??;
        let mut pandoc_ast =
            rocket::tokio::task::spawn_blocking(move || Pandoc::from_json(&pandoc_ast)).await?;

        for filter in filters::FILTERS {
            filter(&mut pandoc_ast, article_mgr);
        }
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

        let pandoc_ast = pandoc_ast.to_json();

        let content = rocket::tokio::task::spawn_blocking({
            move || -> Result<_, error::ArticleError> {
                let mut pandoc = Command::new("pandoc")
                    .args(["-f", "json", "-t", "html"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()?;

                pandoc
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(pandoc_ast.as_bytes())?;
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

        Ok(Article { content, meta })
    }
}

const DEFAULT_TITLE: &dyn Fn() -> String = &|| "Untitled Page".to_string();
const DEFAULT_TEMPLATE: &dyn Fn() -> String = &|| "article".to_string();

#[derive(Serialize, Deserialize, Clone)]
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
    #[serde(flatten)]
    pub extra: Value,
}

#[derive(Serialize, Deserialize, Clone)]
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
        let mut path = Path::new("./articles").join(path);
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
