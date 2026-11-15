use serde::{Deserialize, Serialize};

/// A filter tree (RFC 8620 §5.5): either a condition or an operator over sub-filters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Filter<C> {
    Operator(FilterOperator<C>),
    Condition(C),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilterOperator<C> {
    pub operator: Operator,
    pub conditions: Vec<Filter<C>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Operator {
    And,
    Or,
    Not,
}

impl<C> Filter<C> {
    pub fn and(conditions: Vec<Filter<C>>) -> Self {
        Self::Operator(FilterOperator {
            operator: Operator::And,
            conditions,
        })
    }
    pub fn or(conditions: Vec<Filter<C>>) -> Self {
        Self::Operator(FilterOperator {
            operator: Operator::Or,
            conditions,
        })
    }
    pub fn not(conditions: Vec<Filter<C>>) -> Self {
        Self::Operator(FilterOperator {
            operator: Operator::Not,
            conditions,
        })
    }
}

impl<C> From<C> for Filter<C> {
    fn from(c: C) -> Self {
        Self::Condition(c)
    }
}

/// A sort comparator (RFC 8620 §5.5). `P` is the sortable-property type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comparator<P> {
    pub property: P,
    #[serde(default = "default_true")]
    pub is_ascending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collation: Option<String>,
    /// Only used with the mail `hasKeyword`-style sorts (RFC 8621 §4.4.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword: Option<String>,
}

fn default_true() -> bool {
    true
}

impl<P> Comparator<P> {
    pub fn asc(property: P) -> Self {
        Self {
            property,
            is_ascending: true,
            collation: None,
            keyword: None,
        }
    }
    pub fn desc(property: P) -> Self {
        Self {
            property,
            is_ascending: false,
            collation: None,
            keyword: None,
        }
    }
}
