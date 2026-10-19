use super::types::NavigationLevel;

#[derive(Debug, Clone)]
pub struct NavigationStack {
    levels: Vec<NavigationLevel>,
}

impl NavigationStack {
    pub fn new() -> Self {
        Self { levels: Vec::new() }
    }

    pub fn push(&mut self, level: NavigationLevel) {
        self.levels.push(level);
    }

    pub fn pop(&mut self) -> Option<NavigationLevel> {
        self.levels.pop()
    }

    pub fn truncate(&mut self, len: usize) {
        self.levels.truncate(len);
    }

    pub fn current(&self) -> Option<&NavigationLevel> {
        self.levels.last()
    }

    pub fn get_level(&self, index: usize) -> Option<&NavigationLevel> {
        self.levels.get(index)
    }

    pub fn get_level_mut(&mut self, index: usize) -> Option<&mut NavigationLevel> {
        self.levels.get_mut(index)
    }

    pub fn levels_len(&self) -> usize {
        self.levels.len()
    }

    pub fn levels(&self) -> &[NavigationLevel] {
        &self.levels
    }
}

impl Default for NavigationStack {
    fn default() -> Self {
        Self::new()
    }
}
