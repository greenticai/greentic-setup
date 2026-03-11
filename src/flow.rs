//! QAFlowSpec builder for multi-step setup with conditional jumps.
//!
//! Converts a flat `FormSpec` into a directed-graph `QAFlowSpec` where
//! questions with `visible_if` expressions become decision branches.

use qa_spec::FormSpec;
use qa_spec::spec::flow::{
    CardMode, DecisionCase, DecisionStep, MessageStep, QAFlowSpec, QuestionStep, StepId, StepSpec,
};
use std::collections::BTreeMap;

fn sid(s: &str) -> StepId {
    s.to_string()
}

/// Build a `QAFlowSpec` from a `FormSpec`, inserting decision steps for
/// questions that have `visible_if` conditions.
///
/// The resulting flow is a directed graph where:
/// - Each question becomes a `StepSpec::Question`
/// - Questions with `visible_if` get a preceding `StepSpec::Decision` that
///   evaluates the condition and either proceeds to the question or skips it
/// - A welcome message step is prepended as the entry point
pub fn build_qa_flow(form_spec: &FormSpec) -> QAFlowSpec {
    let mut steps = BTreeMap::new();
    let mut step_order: Vec<StepId> = Vec::new();

    // Entry: welcome message
    let welcome_id = sid("welcome");
    steps.insert(
        welcome_id.clone(),
        StepSpec::Message(MessageStep {
            mode: CardMode::Text,
            template: form_spec.title.clone(),
            next: None,
        }),
    );
    step_order.push(welcome_id.clone());

    for (idx, question) in form_spec.questions.iter().enumerate() {
        if question.id.is_empty() {
            continue;
        }

        let q_step_id = sid(&format!("q_{}", question.id));

        if let Some(ref expr) = question.visible_if {
            let decision_id = sid(&format!("decide_{}", question.id));
            let skip_target = next_step_id(form_spec, idx + 1);

            steps.insert(
                decision_id.clone(),
                StepSpec::Decision(DecisionStep {
                    cases: vec![DecisionCase {
                        if_expr: expr.clone(),
                        goto: q_step_id.clone(),
                    }],
                    default_goto: Some(skip_target),
                }),
            );
            step_order.push(decision_id);
        }

        let next = next_step_id(form_spec, idx + 1);
        steps.insert(
            q_step_id.clone(),
            StepSpec::Question(QuestionStep {
                question_id: question.id.clone(),
                next: Some(next),
            }),
        );
        step_order.push(q_step_id);
    }

    let end_id = sid("end");
    steps.insert(end_id.clone(), StepSpec::End);
    step_order.push(end_id);

    // Patch welcome → first real step
    if step_order.len() > 2
        && let Some(StepSpec::Message(msg)) = steps.get_mut(&welcome_id)
    {
        msg.next = Some(step_order[1].clone());
    }

    QAFlowSpec {
        id: form_spec.id.clone(),
        title: form_spec.title.clone(),
        version: form_spec.version.clone(),
        entry: welcome_id,
        steps,
        policies: None,
    }
}

fn next_step_id(form_spec: &FormSpec, after_idx: usize) -> StepId {
    for question in form_spec.questions.iter().skip(after_idx) {
        if question.id.is_empty() {
            continue;
        }
        if question.visible_if.is_some() {
            return sid(&format!("decide_{}", question.id));
        }
        return sid(&format!("q_{}", question.id));
    }
    sid("end")
}

/// Build a section-based QAFlowSpec where questions are grouped into
/// named sections with message headers and decision gates.
pub fn build_sectioned_flow(form_spec: &FormSpec, sections: &[FlowSection]) -> QAFlowSpec {
    let mut steps = BTreeMap::new();
    let mut step_chain: Vec<StepId> = Vec::new();

    for (sec_idx, section) in sections.iter().enumerate() {
        let header_id = sid(&format!("section_{sec_idx}"));
        steps.insert(
            header_id.clone(),
            StepSpec::Message(MessageStep {
                mode: CardMode::Text,
                template: section.title.clone(),
                next: None,
            }),
        );
        step_chain.push(header_id);

        for qid in &section.question_ids {
            let Some(question) = form_spec.questions.iter().find(|q| &q.id == qid) else {
                continue;
            };
            let q_step_id = sid(&format!("q_{qid}"));

            if let Some(ref expr) = question.visible_if {
                let decision_id = sid(&format!("decide_{qid}"));
                steps.insert(
                    decision_id.clone(),
                    StepSpec::Decision(DecisionStep {
                        cases: vec![DecisionCase {
                            if_expr: expr.clone(),
                            goto: q_step_id.clone(),
                        }],
                        default_goto: None,
                    }),
                );
                step_chain.push(decision_id);
            }

            steps.insert(
                q_step_id.clone(),
                StepSpec::Question(QuestionStep {
                    question_id: question.id.clone(),
                    next: None,
                }),
            );
            step_chain.push(q_step_id);
        }
    }

    let end_id = sid("end");
    steps.insert(end_id.clone(), StepSpec::End);
    step_chain.push(end_id);

    // Patch next pointers
    for i in 0..step_chain.len().saturating_sub(1) {
        let next = step_chain[i + 1].clone();
        match steps.get_mut(&step_chain[i]) {
            Some(StepSpec::Message(msg)) => msg.next = Some(next),
            Some(StepSpec::Question(q)) => q.next = Some(next),
            Some(StepSpec::Decision(d)) => {
                if d.default_goto.is_none() {
                    d.default_goto = Some(next);
                }
            }
            _ => {}
        }
    }

    let entry = step_chain.first().cloned().unwrap_or_else(|| sid("end"));

    QAFlowSpec {
        id: form_spec.id.clone(),
        title: form_spec.title.clone(),
        version: form_spec.version.clone(),
        entry,
        steps,
        policies: None,
    }
}

/// A named section of questions for sectioned flow building.
#[derive(Clone, Debug)]
pub struct FlowSection {
    pub title: String,
    pub question_ids: Vec<String>,
}

/// Auto-detect sections from question IDs by grouping on the prefix
/// before the first underscore (e.g., `redis_host` → section `redis`).
pub fn auto_sections(form_spec: &FormSpec) -> Vec<FlowSection> {
    let mut sections: Vec<FlowSection> = Vec::new();

    for question in &form_spec.questions {
        if question.id.is_empty() {
            continue;
        }
        let prefix = question
            .id
            .split('_')
            .next()
            .unwrap_or(&question.id)
            .to_string();

        if let Some(section) = sections.last_mut()
            && section.title == prefix
        {
            section.question_ids.push(question.id.clone());
            continue;
        }

        sections.push(FlowSection {
            title: prefix,
            question_ids: vec![question.id.clone()],
        });
    }

    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use qa_spec::{Expr, QuestionSpec, QuestionType};

    fn sample_form_spec() -> FormSpec {
        FormSpec {
            id: "test".into(),
            title: "Test Setup".into(),
            version: "1.0.0".into(),
            description: None,
            presentation: None,
            progress_policy: None,
            secrets_policy: None,
            store: vec![],
            validations: vec![],
            includes: vec![],
            questions: vec![
                QuestionSpec {
                    id: "auth_enabled".into(),
                    kind: QuestionType::Boolean,
                    title: "Enable auth?".into(),
                    title_i18n: None,
                    description: None,
                    description_i18n: None,
                    required: true,
                    choices: None,
                    default_value: None,
                    secret: false,
                    visible_if: None,
                    constraint: None,
                    list: None,
                    computed: None,
                    policy: Default::default(),
                    computed_overridable: false,
                },
                QuestionSpec {
                    id: "auth_token".into(),
                    kind: QuestionType::String,
                    title: "Auth token".into(),
                    title_i18n: None,
                    description: None,
                    description_i18n: None,
                    required: true,
                    choices: None,
                    default_value: None,
                    secret: true,
                    visible_if: Some(Expr::Answer {
                        path: "auth_enabled".to_string(),
                    }),
                    constraint: None,
                    list: None,
                    computed: None,
                    policy: Default::default(),
                    computed_overridable: false,
                },
                QuestionSpec {
                    id: "url".into(),
                    kind: QuestionType::String,
                    title: "API URL".into(),
                    title_i18n: None,
                    description: None,
                    description_i18n: None,
                    required: true,
                    choices: None,
                    default_value: None,
                    secret: false,
                    visible_if: None,
                    constraint: None,
                    list: None,
                    computed: None,
                    policy: Default::default(),
                    computed_overridable: false,
                },
            ],
        }
    }

    #[test]
    fn build_flow_creates_decision_gate_for_visible_if() {
        let spec = sample_form_spec();
        let flow = build_qa_flow(&spec);

        assert_eq!(flow.entry, "welcome");
        assert!(flow.steps.contains_key("decide_auth_token"));
        assert!(flow.steps.contains_key("q_auth_token"));
        assert!(flow.steps.contains_key("q_auth_enabled"));
        assert!(flow.steps.contains_key("q_url"));
        assert!(flow.steps.contains_key("end"));

        match flow.steps.get("decide_auth_token") {
            Some(StepSpec::Decision(d)) => {
                assert_eq!(d.cases.len(), 1);
                assert_eq!(d.cases[0].goto, "q_auth_token");
                assert_eq!(d.default_goto, Some("q_url".to_string()));
            }
            other => panic!("expected Decision, got {other:?}"),
        }
    }

    #[test]
    fn build_flow_no_decision_for_unconditional() {
        let spec = sample_form_spec();
        let flow = build_qa_flow(&spec);

        assert!(!flow.steps.contains_key("decide_auth_enabled"));
        assert!(!flow.steps.contains_key("decide_url"));
    }

    #[test]
    fn auto_sections_groups_by_prefix() {
        let spec = FormSpec {
            id: "sec".into(),
            title: "Sections".into(),
            version: "1".into(),
            description: None,
            presentation: None,
            progress_policy: None,
            secrets_policy: None,
            store: vec![],
            validations: vec![],
            includes: vec![],
            questions: vec![
                QuestionSpec {
                    id: "redis_host".into(),
                    kind: QuestionType::String,
                    title: "Redis Host".into(),
                    title_i18n: None,
                    description: None,
                    description_i18n: None,
                    required: true,
                    choices: None,
                    default_value: None,
                    secret: false,
                    visible_if: None,
                    constraint: None,
                    list: None,
                    computed: None,
                    policy: Default::default(),
                    computed_overridable: false,
                },
                QuestionSpec {
                    id: "redis_port".into(),
                    kind: QuestionType::Integer,
                    title: "Redis Port".into(),
                    title_i18n: None,
                    description: None,
                    description_i18n: None,
                    required: false,
                    choices: None,
                    default_value: Some("6379".into()),
                    secret: false,
                    visible_if: None,
                    constraint: None,
                    list: None,
                    computed: None,
                    policy: Default::default(),
                    computed_overridable: false,
                },
                QuestionSpec {
                    id: "api_url".into(),
                    kind: QuestionType::String,
                    title: "API URL".into(),
                    title_i18n: None,
                    description: None,
                    description_i18n: None,
                    required: true,
                    choices: None,
                    default_value: None,
                    secret: false,
                    visible_if: None,
                    constraint: None,
                    list: None,
                    computed: None,
                    policy: Default::default(),
                    computed_overridable: false,
                },
            ],
        };

        let sections = auto_sections(&spec);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, "redis");
        assert_eq!(sections[0].question_ids, vec!["redis_host", "redis_port"]);
        assert_eq!(sections[1].title, "api");
        assert_eq!(sections[1].question_ids, vec!["api_url"]);
    }

    #[test]
    fn sectioned_flow_has_section_headers() {
        let spec = sample_form_spec();
        let sections = vec![
            FlowSection {
                title: "Authentication".into(),
                question_ids: vec!["auth_enabled".into(), "auth_token".into()],
            },
            FlowSection {
                title: "Connection".into(),
                question_ids: vec!["url".into()],
            },
        ];
        let flow = build_sectioned_flow(&spec, &sections);

        assert!(flow.steps.contains_key("section_0"));
        assert!(flow.steps.contains_key("section_1"));
        assert!(flow.steps.contains_key("q_auth_enabled"));
        assert!(flow.steps.contains_key("decide_auth_token"));
        assert!(flow.steps.contains_key("q_url"));
        assert!(flow.steps.contains_key("end"));
    }
}
