async def test_lists_the_page_interaction_tools(client):
    tools = await client.list_tools()
    names = [t.name for t in tools]
    for expected in ("render", "dom", "eval", "click", "scroll", "screenshot"):
        assert expected in names
    # Deliberately absent: an instance renders one document, so there is
    # nothing to navigate to. See skill/SKILL.md.
    assert "navigate" not in names
