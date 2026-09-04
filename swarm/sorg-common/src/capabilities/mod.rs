use cell_protocol::ExecutionCapabilities;

use crate::records::app_deployment::{RequirementTag, RequirementTags};

/// The outcome of checking a runtime's capabilities against a cell's tag requirements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagRequirement {
    /// All required tags are present.
    Met,
    /// One or more required tags are missing.
    Unmet {
        /// The required tags absent from the capabilities.
        missing: Vec<String>,
    },
}

impl TagRequirement {
    /// Returns whether all required tags were present.
    #[must_use]
    pub fn is_met(&self) -> bool {
        matches!(self, TagRequirement::Met)
    }
}

/// Checks a runtime's capabilities against the given tag requirements, reporting
/// which required tags (if any) are missing.
pub fn check_tag_requirements(
    capabilities: &ExecutionCapabilities,
    requirement_tags: &RequirementTags,
) -> TagRequirement {
    let missing: Vec<String> = requirement_tags
        .as_ref()
        .iter()
        .filter(|req_tag| !fulfills_requirement_tag(capabilities, req_tag))
        .map(|req_tag| req_tag.as_ref().to_owned())
        .collect();

    if missing.is_empty() {
        TagRequirement::Met
    } else {
        TagRequirement::Unmet { missing }
    }
}

fn fulfills_requirement_tag(capabilities: &ExecutionCapabilities, tag: &RequirementTag) -> bool {
    capabilities
        .tags()
        .iter()
        .any(|cap_tag| cap_tag.as_ref() == tag.as_ref())
}

#[cfg(test)]
mod test {
    use cell_protocol::{CapabilityTag, ExecutionCapabilities};

    use crate::RequirementTags;

    use super::{TagRequirement, check_tag_requirements};

    fn test_exec_capabilities(capas: Vec<&'static str>) -> ExecutionCapabilities {
        let capa_tags = capas
            .into_iter()
            .map(CapabilityTag::new)
            .collect::<Vec<_>>();
        ExecutionCapabilities::new(capa_tags)
    }

    #[test]
    fn tag_requirements_check_pass_exact() {
        let test_capas = test_exec_capabilities(vec!["tag_one", "tag_two"]);
        let requirements = RequirementTags::new(vec!["tag_one", "tag_two"]);
        assert!(check_tag_requirements(&test_capas, &requirements).is_met());
    }

    #[test]
    fn tag_requirements_check_pass_more_capas() {
        let test_capas = test_exec_capabilities(vec!["tag_one", "tag_two", "tag_three"]);
        let requirements = RequirementTags::new(vec!["tag_one", "tag_two"]);
        assert!(check_tag_requirements(&test_capas, &requirements).is_met());
    }

    #[test]
    fn tag_requirements_check_fail_missing_one() {
        let test_capas = test_exec_capabilities(vec!["tag_one", "tag_two"]);
        let requirements = RequirementTags::new(vec!["tag_one", "tag_two", "tag_four"]);
        assert_eq!(
            check_tag_requirements(&test_capas, &requirements),
            TagRequirement::Unmet {
                missing: vec!["tag_four".to_owned()]
            }
        );
    }

    #[test]
    fn tag_requirements_check_fail_disjoint() {
        let test_capas = test_exec_capabilities(vec!["tag_one", "tag_two", "tag_three"]);
        let requirements = RequirementTags::new(vec!["tag_one", "tag_two", "tag_four"]);
        assert_eq!(
            check_tag_requirements(&test_capas, &requirements),
            TagRequirement::Unmet {
                missing: vec!["tag_four".to_owned()]
            }
        );
    }

    #[test]
    fn tag_requirements_check_fail_no_capas() {
        let test_capas = test_exec_capabilities(vec![]);
        let requirements = RequirementTags::new(vec!["tag_one", "tag_two", "tag_four"]);
        assert_eq!(
            check_tag_requirements(&test_capas, &requirements),
            TagRequirement::Unmet {
                missing: vec![
                    "tag_one".to_owned(),
                    "tag_two".to_owned(),
                    "tag_four".to_owned()
                ]
            }
        );
    }
}
