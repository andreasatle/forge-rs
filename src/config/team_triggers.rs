//! Static analysis of the team-trigger graph formed by `TeamConfig::trigger`.
//!
//! Computed once at config-load time (see `ForgeConfig::from_file`), not
//! re-derived per trigger evaluation — trigger evaluation itself lives in
//! `crate::services::team_trigger` and only asks "has this trigger fired
//! yet", never "what does the whole graph look like".

use std::collections::{HashMap, HashSet};

use super::{TeamConfig, Trigger};
#[cfg(test)]
use crate::machines::scheduler::NodeKind;

/// Computes the terminal teams among `teams`: those no other team's
/// `Trigger::AfterTeams` list names, i.e. teams nothing else is scheduled to
/// run after.
///
/// Fails if the team-trigger graph formed by `AfterTeams` references contains
/// a cycle (a team whose `after_teams` chain transitively refers back to
/// itself) — such a team could never be scheduled, so this is a config
/// error, not a runtime one.
pub(super) fn compute_terminal_teams(
    teams: &[TeamConfig],
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let after_teams: HashMap<&str, &[String]> = teams
        .iter()
        .filter_map(|team| match &team.trigger {
            Trigger::AfterTeams(names) => Some((team.name.as_str(), names.as_slice())),
            Trigger::Start => None,
        })
        .collect();

    for team in teams {
        detect_cycle(&team.name, &after_teams, &mut Vec::new())?;
    }

    let referenced: HashSet<&str> = after_teams
        .values()
        .flat_map(|names| names.iter().map(String::as_str))
        .collect();

    Ok(teams
        .iter()
        .map(|team| team.name.as_str())
        .filter(|name| !referenced.contains(name))
        .map(String::from)
        .collect())
}

/// Depth-first walk of the `after_teams` graph starting at `name`, tracking
/// the current path so a repeated name means a cycle back to itself.
fn detect_cycle(
    name: &str,
    after_teams: &HashMap<&str, &[String]>,
    path: &mut Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(pos) = path.iter().position(|seen| seen == name) {
        let mut cycle = path[pos..].to_vec();
        cycle.push(name.to_string());
        return Err(format!("team trigger cycle detected: {}", cycle.join(" -> ")).into());
    }
    path.push(name.to_string());
    if let Some(required) = after_teams.get(name) {
        for required_name in *required {
            detect_cycle(required_name, after_teams, path)?;
        }
    }
    path.pop();
    Ok(())
}

#[cfg(test)]
#[path = "team_triggers_tests.rs"]
mod tests;
