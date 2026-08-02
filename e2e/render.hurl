# A page whose content is built by script, rendered end to end: the DOM that
# comes back proves the engine ran the script, not just parsed the markup.
POST {{baseurl}}/tools/render
Content-Type: application/json
{
  "arguments": {
    "html": "<html><body><div id=out></div><script>document.getElementById('out').textContent='rendered by script'</script></body></html>",
    "width": 400,
    "height": 200
  }
}
HTTP 200
[Asserts]
jsonpath "$.content" count == 2
jsonpath "$.content[0].mime_type" == "image/png"
jsonpath "$.content[1].mime_type" == "text/html"
# The text the script wrote, not the markup that was sent: a page that failed to
# load at all would still come back as two parts with these same mime types, so
# without this the test would pass on the engine's own error document.
jsonpath "$.content[1].data" contains "rendered by script"
