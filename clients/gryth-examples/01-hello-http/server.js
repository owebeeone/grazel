// gryth-examples 01: hello http — node built-ins only.
// GRYTH_EXAMPLE_ONESHOT=1 → serve exactly one request, then exit 0 (stage knob).
const http = require("node:http");
const oneshot = process.env.GRYTH_EXAMPLE_ONESHOT === "1";
const server = http.createServer((req, res) => {
  res.writeHead(200, { "Content-Type": "text/plain" });
  res.end("hello from gryth-examples\n");
  if (oneshot) server.close();
});
server.listen(0, "127.0.0.1", () => {
  console.log(`listening port=${server.address().port}`);
});
