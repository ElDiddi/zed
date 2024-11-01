use collections::HashMap;
use gpui::{AnyElement, IntoElement};
use multi_buffer::{Anchor, AnchorRangeExt, MultiBufferRow, MultiBufferSnapshot, ToPoint};
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, ops::Range, sync::Arc};
use sum_tree::{Bias, SeekTarget, SumTree};
use text::Point;
use ui::{IconName, SharedString, WindowContext};

use crate::FoldPlaceholder;

#[derive(Copy, Clone, Default, Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
pub struct CreaseId(usize);

pub struct CreaseMap {
    snapshot: CreaseSnapshot,
    next_id: CreaseId,
    id_to_range: HashMap<CreaseId, Range<Anchor>>,
}

impl CreaseMap {
    pub fn new(snapshot: &MultiBufferSnapshot) -> Self {
        CreaseMap {
            snapshot: CreaseSnapshot::new(snapshot),
            next_id: CreaseId::default(),
            id_to_range: HashMap::default(),
        }
    }
}

#[derive(Clone)]
pub struct CreaseSnapshot {
    creases: SumTree<CreaseItem>,
}

impl CreaseSnapshot {
    pub fn new(snapshot: &MultiBufferSnapshot) -> Self {
        CreaseSnapshot {
            creases: SumTree::new(snapshot),
        }
    }

    /// Returns the first Crease starting on the specified buffer row.
    pub fn query_row<'a>(
        &'a self,
        row: MultiBufferRow,
        snapshot: &'a MultiBufferSnapshot,
    ) -> Option<&'a Crease> {
        let start = snapshot.anchor_before(Point::new(row.0, 0));
        let mut cursor = self.creases.cursor::<ItemSummary>(snapshot);
        cursor.seek(&start, Bias::Left, snapshot);
        while let Some(item) = cursor.item() {
            match Ord::cmp(&item.crease.range().start.to_point(snapshot).row, &row.0) {
                Ordering::Less => cursor.next(snapshot),
                Ordering::Equal => {
                    if item.crease.range().start.is_valid(snapshot) {
                        return Some(&item.crease);
                    } else {
                        cursor.next(snapshot);
                    }
                }
                Ordering::Greater => break,
            }
        }
        None
    }

    pub fn creases_in_range<'a>(
        &'a self,
        range: Range<MultiBufferRow>,
        snapshot: &'a MultiBufferSnapshot,
    ) -> impl 'a + Iterator<Item = &'a Crease> {
        let start = snapshot.anchor_before(Point::new(range.start.0, 0));
        let mut cursor = self.creases.cursor::<ItemSummary>(snapshot);
        cursor.seek(&start, Bias::Left, snapshot);

        std::iter::from_fn(move || {
            while let Some(item) = cursor.item() {
                cursor.next(snapshot);
                let crease_range = item.crease.range();
                let crease_start = crease_range.start.to_point(snapshot);
                let crease_end = crease_range.end.to_point(snapshot);
                if crease_end.row > range.end.0 {
                    continue;
                }
                if crease_start.row >= range.start.0 && crease_end.row < range.end.0 {
                    return Some(&item.crease);
                }
            }
            None
        })
    }

    pub fn crease_items_with_offsets(
        &self,
        snapshot: &MultiBufferSnapshot,
    ) -> Vec<(CreaseId, Range<Point>)> {
        let mut cursor = self.creases.cursor::<ItemSummary>(snapshot);
        let mut results = Vec::new();

        cursor.next(snapshot);
        while let Some(item) = cursor.item() {
            let crease_range = item.crease.range();
            let start_point = crease_range.start.to_point(snapshot);
            let end_point = crease_range.end.to_point(snapshot);
            results.push((item.id, start_point..end_point));
            cursor.next(snapshot);
        }

        results
    }
}

type RenderToggleFn = Arc<
    dyn Send
        + Sync
        + Fn(
            MultiBufferRow,
            bool,
            Arc<dyn Send + Sync + Fn(bool, &mut WindowContext)>,
            &mut WindowContext,
        ) -> AnyElement,
>;
type RenderTrailerFn =
    Arc<dyn Send + Sync + Fn(MultiBufferRow, bool, &mut WindowContext) -> AnyElement>;

#[derive(Clone)]
pub enum Crease {
    Fold {
        range: Range<Anchor>,
        placeholder: FoldPlaceholder,
        render_toggle: RenderToggleFn,
        render_trailer: RenderTrailerFn,
        metadata: Option<CreaseMetadata>,
    },
}

/// Metadata about a [`Crease`], that is used for serialization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreaseMetadata {
    pub icon: IconName,
    pub label: SharedString,
}

impl Crease {
    pub fn new<RenderToggle, ToggleElement, RenderTrailer, TrailerElement>(
        range: Range<Anchor>,
        placeholder: FoldPlaceholder,
        render_toggle: RenderToggle,
        render_trailer: RenderTrailer,
    ) -> Self
    where
        RenderToggle: 'static
            + Send
            + Sync
            + Fn(
                MultiBufferRow,
                bool,
                Arc<dyn Send + Sync + Fn(bool, &mut WindowContext)>,
                &mut WindowContext,
            ) -> ToggleElement
            + 'static,
        ToggleElement: IntoElement,
        RenderTrailer: 'static
            + Send
            + Sync
            + Fn(MultiBufferRow, bool, &mut WindowContext) -> TrailerElement
            + 'static,
        TrailerElement: IntoElement,
    {
        Crease::Fold {
            range,
            placeholder,
            render_toggle: Arc::new(move |row, folded, toggle, cx| {
                render_toggle(row, folded, toggle, cx).into_any_element()
            }),
            render_trailer: Arc::new(move |row, folded, cx| {
                render_trailer(row, folded, cx).into_any_element()
            }),
            metadata: None,
        }
    }

    pub fn with_metadata(self, metadata: CreaseMetadata) -> Self {
        match self {
            Crease::Fold {
                range,
                placeholder,
                render_toggle,
                render_trailer,
                ..
            } => Crease::Fold {
                range,
                placeholder,
                render_toggle,
                render_trailer,
                metadata: Some(metadata),
            },
        }
    }

    pub fn range(&self) -> &Range<Anchor> {
        match self {
            Crease::Fold { range, .. } => range,
        }
    }
}

impl std::fmt::Debug for Crease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Crease::Fold { range, .. } => f
                .debug_struct("Crease::Fold")
                .field("range", range)
                .finish(),
        }
    }
}

#[derive(Clone, Debug)]
struct CreaseItem {
    id: CreaseId,
    crease: Crease,
}

impl CreaseMap {
    pub fn snapshot(&self) -> CreaseSnapshot {
        self.snapshot.clone()
    }

    pub fn insert(
        &mut self,
        creases: impl IntoIterator<Item = Crease>,
        snapshot: &MultiBufferSnapshot,
    ) -> Vec<CreaseId> {
        let mut new_ids = Vec::new();
        self.snapshot.creases = {
            let mut new_creases = SumTree::new(snapshot);
            let mut cursor = self.snapshot.creases.cursor::<ItemSummary>(snapshot);
            for crease in creases {
                let crease_range = crease.range().clone();
                new_creases.append(cursor.slice(&crease_range, Bias::Left, snapshot), snapshot);

                let id = self.next_id;
                self.next_id.0 += 1;
                self.id_to_range.insert(id, crease_range);
                new_creases.push(CreaseItem { crease, id }, snapshot);
                new_ids.push(id);
            }
            new_creases.append(cursor.suffix(snapshot), snapshot);
            new_creases
        };
        new_ids
    }

    pub fn remove(
        &mut self,
        ids: impl IntoIterator<Item = CreaseId>,
        snapshot: &MultiBufferSnapshot,
    ) {
        let mut removals = Vec::new();
        for id in ids {
            if let Some(range) = self.id_to_range.remove(&id) {
                removals.push((id, range.clone()));
            }
        }
        removals.sort_unstable_by(|(a_id, a_range), (b_id, b_range)| {
            AnchorRangeExt::cmp(a_range, b_range, snapshot).then(b_id.cmp(a_id))
        });

        self.snapshot.creases = {
            let mut new_creases = SumTree::new(snapshot);
            let mut cursor = self.snapshot.creases.cursor::<ItemSummary>(snapshot);

            for (id, range) in removals {
                new_creases.append(cursor.slice(&range, Bias::Left, snapshot), snapshot);
                while let Some(item) = cursor.item() {
                    cursor.next(snapshot);
                    if item.id == id {
                        break;
                    } else {
                        new_creases.push(item.clone(), snapshot);
                    }
                }
            }

            new_creases.append(cursor.suffix(snapshot), snapshot);
            new_creases
        };
    }
}

#[derive(Debug, Clone)]
pub struct ItemSummary {
    range: Range<Anchor>,
}

impl Default for ItemSummary {
    fn default() -> Self {
        Self {
            range: Anchor::min()..Anchor::min(),
        }
    }
}

impl sum_tree::Summary for ItemSummary {
    type Context = MultiBufferSnapshot;

    fn zero(_cx: &Self::Context) -> Self {
        Default::default()
    }

    fn add_summary(&mut self, other: &Self, _snapshot: &MultiBufferSnapshot) {
        self.range = other.range.clone();
    }
}

impl sum_tree::Item for CreaseItem {
    type Summary = ItemSummary;

    fn summary(&self, _cx: &MultiBufferSnapshot) -> Self::Summary {
        ItemSummary {
            range: self.crease.range().clone(),
        }
    }
}

/// Implements `SeekTarget` for `Range<Anchor>` to enable seeking within a `SumTree` of `CreaseItem`s.
impl SeekTarget<'_, ItemSummary, ItemSummary> for Range<Anchor> {
    fn cmp(&self, cursor_location: &ItemSummary, snapshot: &MultiBufferSnapshot) -> Ordering {
        AnchorRangeExt::cmp(self, &cursor_location.range, snapshot)
    }
}

impl SeekTarget<'_, ItemSummary, ItemSummary> for Anchor {
    fn cmp(&self, other: &ItemSummary, snapshot: &MultiBufferSnapshot) -> Ordering {
        self.cmp(&other.range.start, snapshot)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use gpui::{div, AppContext};
    use multi_buffer::MultiBuffer;

    #[gpui::test]
    fn test_insert_and_remove_creases(cx: &mut AppContext) {
        let text = "line1\nline2\nline3\nline4\nline5";
        let buffer = MultiBuffer::build_simple(text, cx);
        let snapshot = buffer.read_with(cx, |buffer, cx| buffer.snapshot(cx));
        let mut crease_map = CreaseMap::new(&buffer.read(cx).read(cx));

        // Insert creases
        let creases = [
            Crease::new(
                snapshot.anchor_before(Point::new(1, 0))..snapshot.anchor_after(Point::new(1, 5)),
                FoldPlaceholder::test(),
                |_row, _folded, _toggle, _cx| div(),
                |_row, _folded, _cx| div(),
            ),
            Crease::new(
                snapshot.anchor_before(Point::new(3, 0))..snapshot.anchor_after(Point::new(3, 5)),
                FoldPlaceholder::test(),
                |_row, _folded, _toggle, _cx| div(),
                |_row, _folded, _cx| div(),
            ),
        ];
        let crease_ids = crease_map.insert(creases, &snapshot);
        assert_eq!(crease_ids.len(), 2);

        // Verify creases are inserted
        let crease_snapshot = crease_map.snapshot();
        assert!(crease_snapshot
            .query_row(MultiBufferRow(1), &snapshot)
            .is_some());
        assert!(crease_snapshot
            .query_row(MultiBufferRow(3), &snapshot)
            .is_some());

        // Remove creases
        crease_map.remove(crease_ids, &snapshot);

        // Verify creases are removed
        let crease_snapshot = crease_map.snapshot();
        assert!(crease_snapshot
            .query_row(MultiBufferRow(1), &snapshot)
            .is_none());
        assert!(crease_snapshot
            .query_row(MultiBufferRow(3), &snapshot)
            .is_none());
    }

    #[gpui::test]
    fn test_creases_in_range(cx: &mut AppContext) {
        let text = "line1\nline2\nline3\nline4\nline5\nline6\nline7";
        let buffer = MultiBuffer::build_simple(text, cx);
        let snapshot = buffer.read_with(cx, |buffer, cx| buffer.snapshot(cx));
        let mut crease_map = CreaseMap::new(&snapshot);

        let creases = [
            Crease::new(
                snapshot.anchor_before(Point::new(1, 0))..snapshot.anchor_after(Point::new(1, 5)),
                FoldPlaceholder::test(),
                |_row, _folded, _toggle, _cx| div(),
                |_row, _folded, _cx| div(),
            ),
            Crease::new(
                snapshot.anchor_before(Point::new(3, 0))..snapshot.anchor_after(Point::new(3, 5)),
                FoldPlaceholder::test(),
                |_row, _folded, _toggle, _cx| div(),
                |_row, _folded, _cx| div(),
            ),
            Crease::new(
                snapshot.anchor_before(Point::new(5, 0))..snapshot.anchor_after(Point::new(5, 5)),
                FoldPlaceholder::test(),
                |_row, _folded, _toggle, _cx| div(),
                |_row, _folded, _cx| div(),
            ),
        ];
        crease_map.insert(creases, &snapshot);

        let crease_snapshot = crease_map.snapshot();

        let range = MultiBufferRow(0)..MultiBufferRow(7);
        let creases: Vec<_> = crease_snapshot.creases_in_range(range, &snapshot).collect();
        assert_eq!(creases.len(), 3);

        let range = MultiBufferRow(2)..MultiBufferRow(5);
        let creases: Vec<_> = crease_snapshot.creases_in_range(range, &snapshot).collect();
        assert_eq!(creases.len(), 1);
        assert_eq!(creases[0].range().start.to_point(&snapshot).row, 3);

        let range = MultiBufferRow(0)..MultiBufferRow(2);
        let creases: Vec<_> = crease_snapshot.creases_in_range(range, &snapshot).collect();
        assert_eq!(creases.len(), 1);
        assert_eq!(creases[0].range().start.to_point(&snapshot).row, 1);

        let range = MultiBufferRow(6)..MultiBufferRow(7);
        let creases: Vec<_> = crease_snapshot.creases_in_range(range, &snapshot).collect();
        assert_eq!(creases.len(), 0);
    }
}
