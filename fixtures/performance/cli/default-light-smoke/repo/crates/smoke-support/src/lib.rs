pub fn base() -> i32 {
    1
}

#[cfg(test)]
mod tests {
    #[test]
    fn initial_base_is_one() {
        assert_eq!(super::base(), 1);
    }
}
