async def test_renders_a_page_built_by_script(client):
    # A page whose content is built by script, rendered end to end: the DOM
    # that comes back proves the engine ran the script, not just parsed the
    # markup.
    result = await client.call_tool("render", {
        "html": (
            "<html><body><div id=out></div><script>"
            "document.getElementById('out').textContent='rendered by script'"
            "</script></body></html>"
        ),
        "width": 400,
        "height": 200,
    })
    assert len(result.content) == 2

    screenshot, dom = result.content
    # A screenshot is native MCP ImageContent: its mime type lives in the
    # standard `.mimeType` attribute, not in `_meta["dev.actcore/mime-type"]`
    # — that respelled key is only injected for text blocks, which otherwise
    # have no type of their own to carry it on. Measured directly against
    # this component, not assumed from the earlier hurl-derived mapping.
    assert screenshot.type == "image"
    assert screenshot.mimeType == "image/png"

    assert dom.type == "text"
    assert dom.meta["dev.actcore/mime-type"] == "text/html"
    # The text the script wrote, not the markup that was sent: a page that
    # failed to load at all would still come back as two parts with these
    # same mime types, so without this the test would pass on the engine's
    # own error document.
    assert "rendered by script" in dom.text
