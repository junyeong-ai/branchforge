//! Search engine implementations for tool discovery.

use super::index::{ToolIndex, ToolIndexEntry};

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    #[default]
    Regex,
    Bm25,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub entry: ToolIndexEntry,
    pub score: f64,
}

pub struct SearchEngine {
    mode: SearchMode,
}

impl SearchEngine {
    pub fn new(mode: SearchMode) -> Self {
        Self { mode }
    }

    pub fn regex() -> Self {
        Self::new(SearchMode::Regex)
    }

    pub fn bm25() -> Self {
        Self::new(SearchMode::Bm25)
    }

    pub fn mode(&self) -> SearchMode {
        self.mode
    }

    pub fn search(&self, index: &ToolIndex, query: &str, limit: usize) -> Vec<SearchHit> {
        if query.is_empty() || index.is_empty() {
            return Vec::new();
        }

        match self.mode {
            SearchMode::Regex => self.search_regex(index, query, limit),
            SearchMode::Bm25 => self.search_bm25(index, query, limit),
        }
    }

    fn search_regex(&self, index: &ToolIndex, pattern: &str, limit: usize) -> Vec<SearchHit> {
        let regex = match regex::Regex::new(pattern) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        let mut hits: Vec<SearchHit> = index
            .entries()
            .iter()
            .filter_map(|entry| {
                let text = entry.searchable_text();
                if regex.is_match(&text) {
                    Some(SearchHit {
                        entry: entry.clone(),
                        score: 1.0,
                    })
                } else {
                    None
                }
            })
            .collect();

        hits.truncate(limit);
        hits
    }

    fn search_bm25(&self, index: &ToolIndex, query: &str, limit: usize) -> Vec<SearchHit> {
        let query_terms: Vec<&str> = query.split_whitespace().collect();
        if query_terms.is_empty() {
            return Vec::new();
        }

        let avg_doc_len = index
            .entries()
            .iter()
            .map(|e| e.searchable_text().split_whitespace().count())
            .sum::<usize>() as f64
            / index.len().max(1) as f64;

        let mut hits: Vec<SearchHit> = index
            .entries()
            .iter()
            .map(|entry| {
                let score = self.bm25_score(&entry.searchable_text(), &query_terms, avg_doc_len);
                SearchHit {
                    entry: entry.clone(),
                    score,
                }
            })
            .filter(|hit| hit.score > 0.0)
            .collect();

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(limit);
        hits
    }

    fn bm25_score(&self, text: &str, query_terms: &[&str], avg_doc_len: f64) -> f64 {
        const K1: f64 = 1.2;
        const B: f64 = 0.75;

        let text_lower = text.to_lowercase();
        let words: Vec<&str> = text_lower.split_whitespace().collect();
        let doc_len = words.len() as f64;

        let mut score = 0.0;
        for term in query_terms {
            let term_lower = term.to_lowercase();
            let tf = words
                .iter()
                .filter(|w| w.contains(term_lower.as_str()))
                .count() as f64;

            if tf > 0.0 {
                let idf = 1.0; // Simplified IDF
                let numerator = tf * (K1 + 1.0);
                let denominator = tf + K1 * (1.0 - B + B * (doc_len / avg_doc_len.max(1.0)));
                score += idf * (numerator / denominator);
            }
        }

        score
    }
}

impl Default for SearchEngine {
    fn default() -> Self {
        Self::regex()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::McpToolDefinition;

    fn make_index() -> ToolIndex {
        let mut index = ToolIndex::new();

        let tools = [
            (
                "weather",
                "get_weather",
                "Get current weather for a location",
            ),
            ("weather", "get_forecast", "Get weather forecast for days"),
            ("database", "query", "Execute database query"),
            ("database", "insert", "Insert data into database"),
            ("files", "read_file", "Read file contents"),
        ];

        for (server, name, desc) in tools {
            let tool = McpToolDefinition {
                name: name.to_string(),
                description: desc.to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            };
            index.add(super::super::index::ToolIndexEntry::from_mcp_tool(
                server, &tool,
            ));
        }

        index
    }

    #[test]
    fn test_regex_search_simple() {
        let engine = SearchEngine::regex();
        let index = make_index();

        let hits = engine.search(&index, "weather", 5);
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn test_regex_search_pattern() {
        let engine = SearchEngine::regex();
        let index = make_index();

        let hits = engine.search(&index, "get_.*", 5);
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn test_bm25_search() {
        let engine = SearchEngine::bm25();
        let index = make_index();

        let hits = engine.search(&index, "weather location", 5);
        assert!(!hits.is_empty());
        assert!(hits[0].entry.tool_name.contains("weather"));
    }

    #[test]
    fn test_empty_query() {
        let engine = SearchEngine::regex();
        let index = make_index();

        let hits = engine.search(&index, "", 5);
        assert!(hits.is_empty());
    }

    #[test]
    fn test_invalid_regex() {
        let engine = SearchEngine::regex();
        let index = make_index();

        let hits = engine.search(&index, "[invalid", 5);
        assert!(hits.is_empty());
    }
}
