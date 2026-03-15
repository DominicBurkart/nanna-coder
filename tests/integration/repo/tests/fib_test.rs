use fibonacci_example::fibonacci;

#[test]
fn test_fibonacci_10() {
    assert_eq!(fibonacci(10), vec![0, 1, 1, 2, 3, 5, 8, 13, 21, 34]);
}
