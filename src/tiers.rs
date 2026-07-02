//! Depth-tier policy and budget unit-cost constants.

use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Quick,
    Standard,
    Deep,
}

/// Measured worst-case launch costs from the validated `swarm.py` prototype.
pub const DECOMPOSE_WORST_CASE_COST: f64 = 0.001;
/// Measured worst-case launch costs from the validated `swarm.py` prototype.
pub const WORKER_ROUND_WORST_CASE_COST: f64 = 0.03;
/// Measured worst-case launch costs from the validated `swarm.py` prototype.
pub const VERIFICATION_WORST_CASE_COST: f64 = 0.002;
/// Measured prototype extraction ≈ $0.005, rounded up.
pub const EXTRACT_WORST_CASE_COST: f64 = 0.01;
/// Measured prototype contents fetch ≈ $0.003, rounded up.
pub const CONTENTS_WORST_CASE_COST: f64 = 0.005;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerTask {
    pub subquestion: String,
    pub prompt: String,
    pub refinement: bool,
}

impl Depth {
    pub fn decompose_count(self) -> usize {
        match self {
            Depth::Quick => 0,
            Depth::Standard => 4,
            Depth::Deep => 8,
        }
    }

    pub fn needs_decompose(self) -> bool {
        self.decompose_count() > 0
    }
}

pub fn initial_worker_tasks(
    depth: Depth,
    question: &str,
    subquestions: Vec<String>,
) -> Vec<WorkerTask> {
    match depth {
        Depth::Quick => vec![
            WorkerTask {
                subquestion: question.to_string(),
                prompt: question.to_string(),
                refinement: false,
            },
            WorkerTask {
                subquestion: question.to_string(),
                prompt: format!(
                    "Reformulate this question and search from a different angle before answering: {question}"
                ),
                refinement: false,
            },
        ],
        Depth::Standard | Depth::Deep => subquestions
            .into_iter()
            .map(|subquestion| WorkerTask {
                prompt: subquestion.clone(),
                subquestion,
                refinement: false,
            })
            .collect(),
    }
}

pub fn refinement_tasks(subquestions: Vec<String>) -> Vec<WorkerTask> {
    subquestions
        .into_iter()
        .map(|subquestion| WorkerTask {
            prompt: format!(
                "Refinement pass: previous sources did not support this sub-question. Search again from a different angle and answer with dated, sourced facts: {subquestion}"
            ),
            subquestion,
            refinement: true,
        })
        .collect()
}

pub fn dead_subquestions(
    subquestions: &[String],
    supported_or_partial: &[(String, bool)],
) -> Vec<String> {
    let mut seen: HashMap<&str, bool> = subquestions.iter().map(|s| (s.as_str(), false)).collect();
    for (subquestion, good) in supported_or_partial {
        if *good {
            seen.insert(subquestion.as_str(), true);
        }
    }

    let mut emitted = HashSet::new();
    subquestions
        .iter()
        .filter(|subquestion| !seen.get(subquestion.as_str()).copied().unwrap_or(false))
        .filter(|subquestion| emitted.insert(subquestion.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_produces_two_workers_on_same_question() {
        let tasks = initial_worker_tasks(Depth::Quick, "who regulates x?", vec![]);

        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].subquestion, "who regulates x?");
        assert_eq!(tasks[1].subquestion, "who regulates x?");
        assert!(tasks[1].prompt.contains("different angle"));
    }

    #[test]
    fn deep_refinement_selects_only_dead_subquestions() {
        let subquestions = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let verdicts = vec![("a".to_string(), true), ("b".to_string(), false)];

        assert_eq!(dead_subquestions(&subquestions, &verdicts), vec!["b", "c"]);
    }
}
