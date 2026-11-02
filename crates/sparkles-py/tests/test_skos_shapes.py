"""The built-in SKOS shapes (spec U12) as the Python binding returns them."""

import sparkles


def test_skos_shapes_are_the_built_in_turtle() -> None:
    text = sparkles.skos_shapes()
    assert "<urn:x-sparkles:skos:>" in text
    assert "S14:" in text and "S27:" in text
