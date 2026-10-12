use super::*;

/*
0 1 2 3
w a r p
-------
0     4  << the span for the string "mnemos" is (0, 4)

Spanned {
    item: String::new("mnemos"),  << mnemos string
    span: Span::new(0, 4)       << span
}

or >> String::new("mnemos").spanned(Span::new(0, 4))        */
fn mnemos() -> Spanned<String> {
    String::from("mnemos").spanned(Span::new(0, 4))
}

fn empty() -> Spanned<String> {
    String::new().spanned_unknown()
}

#[test]
fn knows_distances() {
    assert!(mnemos().span.distance() == 4);
    assert!(empty().span.distance() == 0);
}
