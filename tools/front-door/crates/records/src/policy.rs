//! Policy facts shared by admission and launch edges. Effects carry their source.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Boundary {
    Credentials,
    GitIsolation,
    DataIntegrity,
    Authority,
    None,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strength {
    Refuse,
    Queue,
    Warn,
    Off,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rule {
    pub code: String,
    pub boundary: Boundary,
    pub strength: Strength,
    pub knob: String,
    pub source: String,
    pub justification: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Proceed,
    Warn(String),
    Queue(String),
    Refuse(String),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Policy {
    pub rules: Vec<Rule>,
}
impl Policy {
    pub fn check(&self, code: &str, violated: bool) -> Effect {
        let Some(rule) = self.rules.iter().find(|rule| rule.code == code) else {
            return Effect::Warn(format!("unknown policy rule {code}; proceeding"));
        };
        if !violated {
            return Effect::Proceed;
        }
        let message = format!(
            "{}: {} ({} = {:?}, source: {})",
            rule.code, rule.justification, rule.knob, rule.strength, rule.source
        );
        match rule.strength {
            Strength::Off => Effect::Proceed,
            Strength::Warn => Effect::Warn(message),
            // A non-boundary policy can be made strict, but never queues a launch.
            Strength::Queue if rule.boundary == Boundary::None => Effect::Warn(message),
            Strength::Queue => Effect::Queue(message),
            Strength::Refuse => Effect::Refuse(message),
        }
    }
}
