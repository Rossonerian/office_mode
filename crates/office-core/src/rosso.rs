//! ROSSO integration helpers: mapping between ROSSO office titles and the agents that play them.

const DOMAINS: &[(&str, &str)] = &[
    ("frontend", "Frontend"),
    ("backend", "Backend"),
    ("database", "Database"),
    ("devops", "DevOps"),
    ("security", "Security"),
    ("integration", "Integration"),
    ("performance", "Performance"),
    ("qa", "QA"),
    ("research", "Research"),
];

/// ROSSO title for an agy router role (`council-architect`, `manager` + domain, …) or a relay
/// subagent type (`agy-council-architect`, …). None when the role is not a ROSSO one.
pub fn title_for(role: &str, domain: Option<&str>) -> Option<String> {
    let r = role.strip_prefix("agy-").unwrap_or(role);
    let t = match r {
        "council-architect" => "Chief Architect".to_string(),
        "council-advisor" => "Review Seat".to_string(),
        "architect" => "Design Engineer".to_string(),
        "manager" => match domain.map(|d| d.trim().to_ascii_lowercase()) {
            Some(d) if !d.is_empty() => {
                let name = DOMAINS.iter().find(|(k, _)| *k == d).map(|(_, v)| (*v).to_string()).unwrap_or_else(|| {
                    let mut c = d.chars();
                    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
                });
                format!("{name} Head")
            }
            _ => "Department Head".to_string(),
        },
        "worker" => "Technician".to_string(),
        "tester" => "Test Inspector".to_string(),
        "supervisor" => "Chief Inspector".to_string(),
        "git" => "Records Clerk".to_string(),
        _ => return None,
    };
    Some(t)
}

/// The relay subagent type that plays a ROSSO title (inverse of `title_for`).
pub fn agent_type_for(title: &str) -> Option<&'static str> {
    let t = title.trim();
    match t {
        "Chief Architect" => Some("agy-council-architect"),
        "Design Engineer" => Some("agy-architect"),
        "Technician" => Some("agy-worker"),
        "Test Inspector" => Some("agy-tester"),
        "Chief Inspector" => Some("agy-supervisor"),
        "Records Clerk" => Some("agy-git"),
        _ if t.ends_with(" Seat") => Some("agy-council-advisor"),
        _ if t.ends_with(" Head") => Some("agy-manager"),
        _ => None,
    }
}

/// ROSSO desk directory name for an office title: "Chief Inspector" -> "chief-inspector".
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Council round step of a ROSSO document type: Task Card 0, Critique 1, Rebuttal 2, Decision Record 3.
pub fn council_step(doc_type: &str) -> Option<usize> {
    match doc_type {
        "Task Card" => Some(0),
        "Critique" => Some(1),
        "Rebuttal" => Some(2),
        "Decision Record" => Some(3),
        _ => None,
    }
}

/// The council round of the most recent ROSSO job that produced council documents.
#[derive(Debug, Clone, PartialEq)]
pub struct CouncilRound {
    pub job: String,
    /// Which of the four steps have a document (Task Card, Critique, Rebuttal, Decision Record).
    pub reached: [bool; 4],
}

impl CouncilRound {
    /// Highest step reached.
    pub fn current(&self) -> usize {
        (0..4).rev().find(|i| self.reached[*i]).unwrap_or(0)
    }
}

pub fn council_round(scene: &crate::model::Scene) -> Option<CouncilRound> {
    let mut latest: Option<(u64, &str)> = None;
    for r in &scene.reports {
        let (Some(job), Some(ty)) = (scene.report_jobs.get(&r.id), scene.report_types.get(&r.id)) else { continue };
        if council_step(ty).is_some() && latest.map_or(true, |(id, _)| r.id > id) {
            latest = Some((r.id, job.as_str()));
        }
    }
    let (_, job) = latest?;
    let mut reached = [false; 4];
    for r in &scene.reports {
        if scene.report_jobs.get(&r.id).map(String::as_str) == Some(job) {
            if let Some(step) = scene.report_types.get(&r.id).and_then(|t| council_step(t)) {
                reached[step] = true;
            }
        }
    }
    Some(CouncilRound { job: job.to_string(), reached })
}

/// Verdict colour class: 0 = good (green), 1 = needs work (amber), 2 = bad (red).
pub fn verdict_class(v: &str) -> u8 {
    match v {
        "APPROVE" | "PASS" | "SUPPORT" => 0,
        "FAIL" | "OBJECT" => 2,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_for_roles_and_types() {
        assert_eq!(title_for("council-architect", None).as_deref(), Some("Chief Architect"));
        assert_eq!(title_for("agy-council-advisor", None).as_deref(), Some("Review Seat"));
        assert_eq!(title_for("manager", Some("backend")).as_deref(), Some("Backend Head"));
        assert_eq!(title_for("agy-manager", Some("devops")).as_deref(), Some("DevOps Head"));
        assert_eq!(title_for("manager", Some("qa")).as_deref(), Some("QA Head"));
        assert_eq!(title_for("worker", None).as_deref(), Some("Technician"));
        assert_eq!(title_for("agy-git", None).as_deref(), Some("Records Clerk"));
        assert_eq!(title_for("Explore", None), None);
        assert_eq!(title_for("fallback", None), None);
    }

    #[test]
    fn inverse_mapping() {
        assert_eq!(agent_type_for("Chief Architect"), Some("agy-council-architect"));
        assert_eq!(agent_type_for("Security Seat"), Some("agy-council-advisor"));
        assert_eq!(agent_type_for("Frontend Head"), Some("agy-manager"));
        assert_eq!(agent_type_for("Test Inspector"), Some("agy-tester"));
        assert_eq!(agent_type_for("Chief of Staff"), None);
        assert_eq!(agent_type_for("CEO"), None);
    }
}
