use std::{
    ops::Bound,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use crate::article::{ArticleManager, Search};
use chrono::{Local, NaiveDate};
use pandoc_ast::{Attr, Block, Format, Inline, MutVisitor, Pandoc};
use rocket::tokio::{
    runtime::{Handle, Runtime},
    task::spawn_blocking,
};
use rocket_dyn_templates::{
    context,
    tera::{Context, Tera},
    Template,
};
use serde::{Deserialize, Serialize};

lazy_static::lazy_static! {
    static ref TERA: Tera = {
        let mut tera = Tera::default();
        let files = walkdir::WalkDir::new("./templates").into_iter().flatten().filter(|f| f.file_type().is_file()).map(|file| {
            (file.path().to_path_buf(), Some(file.file_name().to_string_lossy().trim_end_matches(".html.tera").to_string()))
        });
        tera.add_template_files(files).unwrap();
        tera
    };
}

pub async fn apply_filters(
    my_path: Arc<PathBuf>,
    ast: Pandoc,
    article_manager: Arc<ArticleManager>,
) -> Pandoc {
    let ast = frag_search_results(my_path.clone(), ast, article_manager).await;
    ast
}

async fn frag_search_results(
    my_path: Arc<PathBuf>,
    mut ast: Pandoc,
    article_manager: Arc<ArticleManager>,
) -> Pandoc {
    let has_any_searches = Arc::new(AtomicBool::new(false));
    struct FragSearchVisitor(Arc<ArticleManager>, Handle, Arc<PathBuf>, Arc<AtomicBool>);
    impl MutVisitor for FragSearchVisitor {
        fn visit_block(&mut self, block: &mut Block) {
            if let Block::CodeBlock((_, classes, _), contents) = block {
                self.3.store(true, Ordering::Relaxed);
                if !dbg!(&classes).iter().any(|c| c == "search") {
                    return;
                }

                let Ok(mut search): Result<Search, _> = serde_yml::from_str(contents) else {
                    eprintln!("Bad search block {contents}");
                    return;
                };
                search.exclude_paths.push(self.2.as_ref().clone());

                let Ok(search) = self.1.block_on(self.0.clone().search(&search)) else {
                    eprintln!("Search failed: {search:#?}");
                    return;
                };

                let ctx = context! {
                    articles: search
                };
                let ctx = Context::from_serialize(ctx).unwrap();

                let html = TERA
                    .render("frag-search-results", &ctx)
                    .unwrap_or_else(|e| format!("Search template failure: {e:#?}"));
                *block = Block::RawBlock(Format("html".to_string()), html);
            }
        }
    }
    let initial = ast.clone();
    let mut visitor = FragSearchVisitor(
        article_manager.clone(),
        Handle::current(),
        my_path,
        has_any_searches.clone(),
    );
    let Ok(mut ast) = spawn_blocking(move || {
        visitor.walk_pandoc(&mut ast);
        ast
    })
    .await
    else {
        eprintln!("Filter failed");
        return initial;
    };
    if has_any_searches.load(Ordering::Relaxed) {
        ast.meta.insert(
            "always_rerender".to_string(),
            pandoc_ast::MetaValue::MetaBool(true),
        );
    }
    ast
}
