use std::path::PathBuf;
use std::sync::Arc;

use futures::FutureExt;
use futures::future::Shared;
use ordered_float::OrderedFloat;
use uuid::Uuid;
use warp_terminal::local_tty::persistent::{Runtime, SessionState};
use warpui::elements::Text;
use warpui::{AppContext, Element, SingletonEntity};

use super::mixer::CommandPaletteItemAction;
use super::render_util::render_search_item_icon;
use crate::appearance::Appearance;
use crate::search::data_source::{DataSourceSearchError, Query, QueryResult};
use crate::search::item::SearchItem;
use crate::search::mixer::{AsyncDataSource, BoxFuture, DataSourceRunErrorWrapper};
use crate::search::result_renderer::ItemHighlightState;
use crate::ui_components::icons::Icon;

type SessionSnapshot = Result<Arc<Vec<DetachedSession>>, DataSourceSearchError>;

pub(super) struct DetachedSessionDataSource {
    sessions: Shared<BoxFuture<'static, SessionSnapshot>>,
}

impl DetachedSessionDataSource {
    pub(super) fn new(path: PathBuf) -> Self {
        let sessions = async move {
            tokio::task::spawn_blocking(move || {
                let mut sessions: Vec<_> = Runtime::new(path)?
                    .list()?
                    .into_iter()
                    .filter(|session| session.state == SessionState::Detached)
                    .map(|session| {
                        let id = session.id;
                        let short_id = id.simple().to_string()[..8].to_owned();
                        DetachedSession {
                            id,
                            label: match session.name {
                                Some(name) => name,
                                None => format!("Terminal session · {short_id}"),
                            },
                            rank: 0,
                        }
                    })
                    .collect();
                sessions.sort_by(|a, b| a.label.cmp(&b.label));
                for (rank, session) in sessions.iter_mut().enumerate() {
                    session.rank = rank;
                }
                Ok::<_, anyhow::Error>(Arc::new(sessions))
            })
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or_else(|| DataSourceSearchError::new("Could not list detached sessions".into()))
        }
        .boxed()
        .shared();
        Self { sessions }
    }
}

impl AsyncDataSource for DetachedSessionDataSource {
    type Action = CommandPaletteItemAction;

    fn run_query(
        &self,
        query: &Query,
        _: &AppContext,
    ) -> BoxFuture<'static, Result<Vec<QueryResult<Self::Action>>, DataSourceRunErrorWrapper>> {
        let sessions = self.sessions.clone();
        let query = query.text.to_lowercase();
        async move {
            let sessions = sessions
                .await
                .map_err(|err| -> DataSourceRunErrorWrapper { Box::new(err) })?;
            Ok(sessions
                .iter()
                .filter(|session| {
                    session.label.to_lowercase().contains(&query)
                        || session.id.to_string().contains(&query)
                        || session.id.simple().to_string().contains(&query)
                })
                .cloned()
                .map(QueryResult::from)
                .collect())
        }
        .boxed()
    }
}

#[derive(Clone)]
struct DetachedSession {
    id: Uuid,
    label: String,
    rank: usize,
}

impl SearchItem for DetachedSession {
    type Action = CommandPaletteItemAction;

    fn render_icon(
        &self,
        highlight_state: ItemHighlightState,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        render_search_item_icon(
            appearance,
            Icon::Terminal,
            highlight_state.icon_fill(appearance).into_solid(),
            highlight_state,
        )
    }

    fn render_item(
        &self,
        highlight_state: ItemHighlightState,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        Text::new_inline(
            self.label.clone(),
            appearance.ui_font_family(),
            appearance.monospace_font_size(),
        )
        .with_color(highlight_state.main_text_fill(appearance).into_solid())
        .finish()
    }

    fn score(&self) -> OrderedFloat<f64> {
        OrderedFloat(-(self.rank as f64))
    }

    fn accept_result(&self) -> Self::Action {
        CommandPaletteItemAction::AttachPersistentSession(self.id)
    }

    fn execute_result(&self) -> Self::Action {
        self.accept_result()
    }

    fn accessibility_label(&self) -> String {
        self.label.clone()
    }
}
