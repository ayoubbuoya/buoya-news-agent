module.exports = {
  apps: [{
    name: "buoya",
    cwd: "/home/enda/buoya/buoya-news-agent",
    script: "./target/release/buoya-news-agent",
    args: "serve --port 8095",
    autorestart: true,
    max_restarts: 10,
  }]
}