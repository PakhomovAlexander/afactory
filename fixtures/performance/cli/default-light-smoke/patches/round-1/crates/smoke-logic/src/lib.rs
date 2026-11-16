pub fn calculate() -> i32 {
    smoke_support::base() + 2
}

#[cfg(test)]
mod tests {
    #[test]
    fn repaired_calculation_extends_support_by_two() {
        assert_eq!(super::calculate(), smoke_support::base() + 2);
    }
}
