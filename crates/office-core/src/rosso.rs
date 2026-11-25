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
