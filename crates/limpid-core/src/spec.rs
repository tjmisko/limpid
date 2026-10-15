pub const MAX_RANGE: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnSpec {
    Single(usize),
    Range {
        start: Option<usize>,
        end: Option<usize>,
        inclusive: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTurnSpec {
    pub kind: &'static str,
    pub indices: Vec<usize>,
}

pub fn parse(input: &str) -> Result<TurnSpec, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(TurnSpec::Single(0));
    }
    if input.bytes().all(|byte| byte.is_ascii_digit()) {
        return input
            .parse()
            .map(TurnSpec::Single)
            .map_err(|_| invalid(input));
    }
    let Some((left, right)) = input.split_once("..") else {
        return Err(invalid(input));
    };
    let (inclusive, right) = match right.strip_prefix('=') {
        Some(right) => (true, right),
        None => (false, right),
    };
    if (!left.is_empty() && !left.bytes().all(|byte| byte.is_ascii_digit()))
        || (!right.is_empty() && !right.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(invalid(input));
    }
    let start = if left.is_empty() {
        None
    } else {
        Some(left.parse().map_err(|_| invalid(input))?)
    };
    let end = if right.is_empty() {
        None
    } else {
        Some(right.parse().map_err(|_| invalid(input))?)
    };
    Ok(TurnSpec::Range {
        start,
        end,
        inclusive,
    })
}

fn invalid(input: &str) -> String {
    format!("Invalid argument \"{input}\". Use 0, 3, 0..2, 1..=3, 2.., ..4, or ..")
}

pub fn resolve(spec: &TurnSpec, total: usize) -> Result<ResolvedTurnSpec, String> {
    if total == 0 {
        return Err("No response to read yet.".into());
    }
    match spec {
        TurnSpec::Single(index) => {
            if *index >= total {
                return Err(format!(
                    "Turn {index} does not exist (only {total} available)."
                ));
            }
            Ok(ResolvedTurnSpec {
                kind: "single",
                indices: vec![*index],
            })
        }
        TurnSpec::Range {
            start,
            end,
            inclusive,
        } => {
            let start = start.unwrap_or(0);
            let requested_end = match end {
                Some(end) => end
                    .checked_add(usize::from(*inclusive))
                    .ok_or_else(|| "Range bound is too large.".to_owned())?,
                None => total,
            };
            if start >= total {
                return Err(format!(
                    "Turn {start} does not exist (only {total} available)."
                ));
            }
            if start >= requested_end {
                return Err("Empty range.".into());
            }
            let count = requested_end - start;
            if count > MAX_RANGE {
                return Err(format!(
                    "Range covers {count} turns; the maximum is {MAX_RANGE}."
                ));
            }
            Ok(ResolvedTurnSpec {
                kind: "range",
                indices: (start..requested_end.min(total)).collect(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_parity() {
        assert_eq!(parse(""), Ok(TurnSpec::Single(0)));
        assert_eq!(parse(" 2 "), Ok(TurnSpec::Single(2)));
        assert_eq!(
            parse("1..=3"),
            Ok(TurnSpec::Range {
                start: Some(1),
                end: Some(3),
                inclusive: true
            })
        );
        assert_eq!(
            parse(".."),
            Ok(TurnSpec::Range {
                start: None,
                end: None,
                inclusive: false
            })
        );
        for bad in ["abc", "-1", "1..2..3", "1...3", "=3", "1.5", "0..x"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert_eq!(
            resolve(&parse("..=9").unwrap(), 12).unwrap().indices,
            (0..10).collect::<Vec<_>>()
        );
        assert_eq!(
            resolve(&parse("0..11").unwrap(), 3).unwrap_err(),
            "Range covers 11 turns; the maximum is 10."
        );
        assert_eq!(
            resolve(&parse("2..").unwrap(), 5).unwrap().indices,
            vec![2, 3, 4]
        );
        assert_eq!(
            resolve(&parse("0..8").unwrap(), 3).unwrap().indices,
            vec![0, 1, 2]
        );
        assert_eq!(
            resolve(&parse("0").unwrap(), 0).unwrap_err(),
            "No response to read yet."
        );
    }
}
