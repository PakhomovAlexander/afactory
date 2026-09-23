//! The left bar: a folding tree whose depth-1 folders are the four fixed tabs, with vim folds,
//! motion and search over it.

/// The four fixed folders under the scope root, in bar order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Tab {
    Providers,
    Workers,
    Pipelines,
    Tasks,
}

impl Tab {
    pub(crate) const ALL: [Tab; 4] = [Tab::Providers, Tab::Workers, Tab::Pipelines, Tab::Tasks];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Tab::Providers => "providers",
            Tab::Workers => "workers",
            Tab::Pipelines => "pipelines",
            Tab::Tasks => "tasks",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NodeKind {
    Root,
    Folder(Tab),
    Item(Tab),
}

/// One entry a pane contributes under its folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Item {
    /// Stable identity within the folder: what `Enter` opens.
    pub(crate) id: String,
    pub(crate) label: String,
    /// Discovered but not selectable, like an ambient Provider candidate.
    pub(crate) muted: bool,
}

#[derive(Clone, Debug)]
struct Node {
    kind: NodeKind,
    id: String,
    label: String,
    muted: bool,
    folded: bool,
    children: Vec<Node>,
}

impl Node {
    fn folder(tab: Tab) -> Node {
        Node {
            kind: NodeKind::Folder(tab),
            id: tab.name().to_owned(),
            label: format!("{}/", tab.name()),
            muted: false,
            folded: false,
            children: Vec::new(),
        }
    }

    fn item(tab: Tab, item: Item) -> Node {
        Node {
            kind: NodeKind::Item(tab),
            id: item.id,
            label: item.label,
            muted: item.muted,
            folded: false,
            children: Vec::new(),
        }
    }
}

/// One visible bar row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) depth: usize,
    pub(crate) kind: NodeKind,
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) muted: bool,
    /// `Some(folded)` for the root and the folders, `None` for an item.
    pub(crate) fold: Option<bool>,
    /// Child indices from the root to this node.
    path: Vec<usize>,
}

impl Row {
    /// The bar text: two columns of indent per level, then `v` or `>` for a folder.
    pub(crate) fn text(&self) -> String {
        let glyph = match self.fold {
            Some(false) => 'v',
            Some(true) => '>',
            None => ' ',
        };
        format!("{}{glyph} {}", "  ".repeat(self.depth), self.label)
    }
}

pub(crate) struct Tree {
    root: Node,
    cursor: usize,
}

impl Tree {
    pub(crate) fn new(root: &str) -> Tree {
        let root = Node {
            kind: NodeKind::Root,
            id: String::new(),
            label: root.to_owned(),
            muted: false,
            folded: false,
            children: Tab::ALL.into_iter().map(Node::folder).collect(),
        };
        Tree { root, cursor: 0 }
    }

    pub(crate) fn set_root(&mut self, label: &str) {
        self.root.label = label.to_owned();
    }

    /// Replace one folder's entries, keeping the cursor on the node it was on when that node
    /// is still there.
    pub(crate) fn set_items(&mut self, tab: Tab, items: Vec<Item>) {
        let selected = self.selected();
        let wanted = NodeKind::Folder(tab);
        let folders = &mut self.root.children;
        if let Some(folder) = folders.iter_mut().find(|node| node.kind == wanted) {
            folder.children = items.into_iter().map(|it| Node::item(tab, it)).collect();
        }
        let rows = self.rows();
        let same = |row: &Row| row.kind == selected.kind && row.id == selected.id;
        match rows.iter().position(same) {
            Some(index) => self.cursor = index,
            None => self.focus(&selected.path),
        }
    }

    pub(crate) fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        visit(&self.root, 0, &mut Vec::new(), &mut rows);
        rows
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn selected(&self) -> Row {
        let mut rows = self.rows();
        let index = self.cursor.min(rows.len() - 1);
        rows.swap_remove(index)
    }

    pub(crate) fn move_by(&mut self, delta: isize) {
        let last = self.rows().len() - 1;
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    pub(crate) fn top(&mut self) {
        self.cursor = 0;
    }

    pub(crate) fn bottom(&mut self) {
        self.cursor = self.rows().len() - 1;
    }

    /// `zo`: open the folder under the cursor.
    pub(crate) fn fold_open(&mut self) {
        let row = self.selected();
        if row.fold.is_some() {
            self.node(&row.path).folded = false;
        }
    }

    /// `zc`: close the folder under the cursor, or the one an item sits in.
    pub(crate) fn fold_close(&mut self) {
        let row = self.selected();
        let target = match row.fold {
            Some(_) => row.path,
            None => parent(&row.path).to_vec(),
        };
        self.close(&target);
    }

    /// `za`: toggle the folder under the cursor; on an item, close the folder it sits in.
    pub(crate) fn fold_toggle(&mut self) {
        let row = self.selected();
        match row.fold {
            Some(true) => self.node(&row.path).folded = false,
            Some(false) => self.close(&row.path),
            None => self.close(parent(&row.path)),
        }
    }

    /// `zR` and `zM`: every folder open or closed. The scope root stays open, so the four
    /// folders are always visible.
    pub(crate) fn fold_all(&mut self, folded: bool) {
        let selected = self.selected();
        for folder in &mut self.root.children {
            folder.folded = folded;
        }
        self.focus(&selected.path);
    }

    /// `h`: close an open folder, otherwise step to the parent.
    pub(crate) fn left(&mut self) {
        let row = self.selected();
        if row.fold == Some(false) && !row.path.is_empty() {
            self.close(&row.path);
        } else if !row.path.is_empty() {
            self.focus(parent(&row.path));
        }
    }

    /// `l`: open a closed folder, or step into an open one. On an item it returns `true`: the
    /// caller opens the item, as `l` opens a file in a file tree.
    pub(crate) fn right(&mut self) -> bool {
        let row = self.selected();
        match row.fold {
            Some(true) => self.node(&row.path).folded = false,
            Some(false) => {
                let rows = self.rows();
                let child = rows.get(self.cursor + 1);
                if child.is_some_and(|child| child.depth > row.depth) {
                    self.cursor += 1;
                }
            }
            None => return true,
        }
        false
    }

    /// `]]`: the next of the four folders.
    pub(crate) fn next_folder(&mut self) {
        let rows = self.rows();
        let start = (self.cursor + 1).min(rows.len());
        if let Some(offset) = rows[start..].iter().position(is_folder) {
            self.cursor = start + offset;
        }
    }

    /// `[[`: the previous of the four folders.
    pub(crate) fn previous_folder(&mut self) {
        let rows = self.rows();
        let end = self.cursor.min(rows.len());
        if let Some(index) = rows[..end].iter().rposition(is_folder) {
            self.cursor = index;
        }
    }

    /// `/` and `?`: the next node whose label contains `query`, ignoring case, wrapping around.
    /// Folded nodes are searched too, and the folders around a match open.
    pub(crate) fn search(&mut self, query: &str, forward: bool) -> bool {
        let query = query.to_ascii_lowercase();
        if query.is_empty() {
            return false;
        }
        let mut all = Vec::new();
        preorder(&self.root, &mut Vec::new(), &mut all);
        let current = self.selected().path;
        let start = all.iter().position(|(path, _)| *path == current);
        let start = start.unwrap_or(0);
        let count = all.len();
        for step in 1..=count {
            let index = if forward {
                (start + step) % count
            } else {
                (start + count - step) % count
            };
            if all[index].1.to_ascii_lowercase().contains(&query) {
                let path = all[index].0.clone();
                for length in 0..path.len() {
                    self.node(&path[..length]).folded = false;
                }
                self.focus(&path);
                return true;
            }
        }
        false
    }

    fn close(&mut self, path: &[usize]) {
        if path.is_empty() {
            return;
        }
        self.node(path).folded = true;
        self.focus(path);
    }

    fn node(&mut self, path: &[usize]) -> &mut Node {
        let mut node = &mut self.root;
        for index in path {
            node = &mut node.children[*index];
        }
        node
    }

    /// Put the cursor on the node at `path`, or on its nearest visible ancestor.
    fn focus(&mut self, path: &[usize]) {
        let rows = self.rows();
        for length in (0..=path.len()).rev() {
            let prefix = &path[..length];
            if let Some(index) = rows.iter().position(|row| row.path == prefix) {
                self.cursor = index;
                return;
            }
        }
        self.cursor = 0;
    }
}

fn parent(path: &[usize]) -> &[usize] {
    &path[..path.len().saturating_sub(1)]
}

fn is_folder(row: &Row) -> bool {
    matches!(row.kind, NodeKind::Folder(_))
}

fn visit(node: &Node, depth: usize, path: &mut Vec<usize>, rows: &mut Vec<Row>) {
    rows.push(Row {
        depth,
        kind: node.kind,
        id: node.id.clone(),
        label: node.label.clone(),
        muted: node.muted,
        fold: match node.kind {
            NodeKind::Item(_) => None,
            NodeKind::Root | NodeKind::Folder(_) => Some(node.folded),
        },
        path: path.clone(),
    });
    if node.folded {
        return;
    }
    for (index, child) in node.children.iter().enumerate() {
        path.push(index);
        visit(child, depth + 1, path, rows);
        path.pop();
    }
}

fn preorder(node: &Node, path: &mut Vec<usize>, all: &mut Vec<(Vec<usize>, String)>) {
    all.push((path.clone(), node.label.clone()));
    for (index, child) in node.children.iter().enumerate() {
        path.push(index);
        preorder(child, path, all);
        path.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> Item {
        Item {
            id: id.to_owned(),
            label: id.to_owned(),
            muted: false,
        }
    }

    fn tree() -> Tree {
        let mut tree = Tree::new("hub  (project)");
        let providers = vec![item("claude-main"), item("codex-main")];
        tree.set_items(Tab::Providers, providers);
        let pipelines = vec![item("review"), item("fixture/implementation")];
        tree.set_items(Tab::Pipelines, pipelines);
        tree
    }

    fn texts(tree: &Tree) -> Vec<String> {
        tree.rows().iter().map(Row::text).collect()
    }

    fn selected(tree: &Tree) -> String {
        tree.selected().label
    }

    #[test]
    fn the_bar_lists_the_four_folders_under_the_scope_root() {
        let expected = [
            "v hub  (project)",
            "  v providers/",
            "      claude-main",
            "      codex-main",
            "  v workers/",
            "  v pipelines/",
            "      review",
            "      fixture/implementation",
            "  v tasks/",
        ];
        assert_eq!(texts(&tree()), expected);
    }

    #[test]
    fn motion_moves_by_rows_pages_and_folders() {
        let mut tree = tree();
        tree.move_by(1);
        assert_eq!(selected(&tree), "providers/");
        tree.move_by(-5);
        assert_eq!(selected(&tree), "hub  (project)");
        tree.move_by(100);
        assert_eq!(selected(&tree), "tasks/");
        tree.top();
        let mut folders = Vec::new();
        for _ in 0..4 {
            tree.next_folder();
            folders.push(selected(&tree));
        }
        let expected = ["providers/", "workers/", "pipelines/", "tasks/"];
        assert_eq!(folders, expected);
        tree.next_folder();
        assert_eq!(selected(&tree), "tasks/");
        tree.previous_folder();
        tree.previous_folder();
        assert_eq!(selected(&tree), "workers/");
        tree.bottom();
        tree.previous_folder();
        assert_eq!(selected(&tree), "pipelines/");
    }

    #[test]
    fn folds_open_close_and_keep_the_cursor_visible() {
        let mut tree = tree();
        tree.move_by(2);
        assert_eq!(selected(&tree), "claude-main");
        // `zc` on an item closes the folder it sits in and lands on that folder.
        tree.fold_close();
        assert_eq!(selected(&tree), "providers/");
        assert_eq!(texts(&tree)[1], "  > providers/");
        assert_eq!(tree.rows().len(), 7);
        tree.fold_toggle();
        assert_eq!(tree.rows().len(), 9);
        tree.fold_toggle();
        assert_eq!(tree.rows().len(), 7);
        tree.fold_open();
        assert_eq!(tree.rows().len(), 9);
        // `zM` keeps the root open and the cursor on the nearest visible ancestor.
        tree.move_by(6);
        assert_eq!(selected(&tree), "fixture/implementation");
        tree.fold_all(true);
        assert_eq!(tree.rows().len(), 5);
        assert_eq!(selected(&tree), "pipelines/");
        tree.fold_all(false);
        assert_eq!(tree.rows().len(), 9);
        assert_eq!(selected(&tree), "pipelines/");
        // The scope root never folds.
        tree.top();
        tree.fold_close();
        assert_eq!(tree.rows().len(), 9);
    }

    #[test]
    fn h_and_l_close_open_step_and_open_items() {
        let mut tree = tree();
        tree.move_by(1);
        assert!(!tree.right());
        assert_eq!(selected(&tree), "claude-main");
        assert!(tree.right(), "l on an item asks the caller to open it");
        tree.left();
        assert_eq!(selected(&tree), "providers/");
        tree.left();
        assert_eq!(texts(&tree)[1], "  > providers/");
        assert!(!tree.right());
        assert_eq!(texts(&tree)[1], "  v providers/");
        tree.left();
        tree.left();
        assert_eq!(selected(&tree), "hub  (project)");
    }

    #[test]
    fn search_wraps_opens_folds_and_runs_both_ways() {
        let mut tree = tree();
        tree.fold_all(true);
        assert!(tree.search("IMPL", true));
        assert_eq!(selected(&tree), "fixture/implementation");
        assert_eq!(texts(&tree)[3], "  v pipelines/");
        assert!(tree.search("main", true));
        assert_eq!(selected(&tree), "claude-main");
        assert!(tree.search("main", true));
        assert_eq!(selected(&tree), "codex-main");
        assert!(tree.search("main", false));
        assert_eq!(selected(&tree), "claude-main");
        assert!(tree.search("main", false));
        assert_eq!(selected(&tree), "codex-main");
        assert!(!tree.search("nothing", true));
        assert_eq!(selected(&tree), "codex-main");
    }

    #[test]
    fn replacing_items_keeps_the_cursor_on_its_node() {
        let mut tree = tree();
        tree.move_by(3);
        assert_eq!(selected(&tree), "codex-main");
        let providers = vec![item("a"), item("claude-main"), item("codex-main")];
        tree.set_items(Tab::Providers, providers);
        assert_eq!(selected(&tree), "codex-main");
        tree.set_items(Tab::Providers, Vec::new());
        assert_eq!(selected(&tree), "providers/");
    }
}
