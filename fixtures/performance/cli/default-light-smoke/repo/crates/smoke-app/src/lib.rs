pub fn value() -> i32 {
    smoke_logic::calculate()
}

#[cfg(test)]
mod tests {
    #[test]
    fn initial_value_is_two() {
        assert_eq!(super::value(), 2);
    }
}
