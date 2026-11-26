//! Claim types shared by the writers (Task 9) and the verifier (Tasks 10 and 11).
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    Number,
    Date,
    Quote,
    Fact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeriveOp {
    Sum,
    Difference,
    Ratio,
    Share,
    PercentChange,
}

/// A figure computed from cited values: `args` index the claim's `cite` list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Derive {
    pub op: DeriveOp,
    pub args: Vec<u8>,
}

impl Derive {
    /// `Sum` takes two or more arguments, every other operation exactly two.
    pub fn arity_ok(&self) -> bool {
        match self.op {
            DeriveOp::Sum => self.args.len() >= 2,
            _ => self.args.len() == 2,
        }
    }

    /// The arity is right and every argument indexes a `cite` list of `cite_len` entries.
    pub fn indices_in(&self, cite_len: usize) -> bool {
        self.arity_ok() && self.args.iter().all(|&i| usize::from(i) < cite_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claim_types_serde_wire_names() {
        let kinds = [
            (ClaimKind::Number, "number"),
            (ClaimKind::Date, "date"),
            (ClaimKind::Quote, "quote"),
            (ClaimKind::Fact, "fact"),
        ];
        for (kind, wire) in kinds {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(wire));
        }
        let ops = [
            (DeriveOp::Sum, "sum"),
            (DeriveOp::Difference, "difference"),
            (DeriveOp::Ratio, "ratio"),
            (DeriveOp::Share, "share"),
            (DeriveOp::PercentChange, "percent_change"),
        ];
        for (op, wire) in ops {
            assert_eq!(serde_json::to_value(op).unwrap(), json!(wire));
        }
        let derive: Derive = serde_json::from_value(json!({"op":"sum","args":[0,1]})).unwrap();
        assert_eq!(derive.args, vec![0, 1]);
        assert!(serde_json::from_value::<Derive>(json!({"op":"sum","args":[300]})).is_err());
        assert!(serde_json::from_value::<Derive>(json!({"op":"sum","args":[0,1],"x":1})).is_err());
    }

    #[test]
    fn derive_arity() {
        let d = |op, args: &[u8]| Derive {
            op,
            args: args.to_vec(),
        };
        assert!(d(DeriveOp::Sum, &[0, 1, 2]).arity_ok());
        assert!(!d(DeriveOp::Sum, &[0]).arity_ok());
        assert!(d(DeriveOp::Ratio, &[0, 1]).arity_ok());
        assert!(!d(DeriveOp::Ratio, &[0, 1, 2]).arity_ok());
        assert!(d(DeriveOp::Share, &[0, 1]).indices_in(2));
        assert!(!d(DeriveOp::Share, &[0, 2]).indices_in(2));
    }
}
