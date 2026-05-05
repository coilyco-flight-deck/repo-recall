class RepoRecall < Formula
  desc "Local dev dashboard that indexes Claude Code session history against your repos"
  homepage "https://github.com/coilysiren/repo-recall"
  url "https://github.com/coilysiren/repo-recall/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "babdbd48410e70fa65a55a4e13f7008062dcfc4af0033dd376221b58185dfea8"
  license "MIT"
  head "https://github.com/coilysiren/repo-recall.git", branch: "main"

  depends_on "rust" => :build

  def install
    # Cargo.toml is pinned at 0.0.0-dev; build.rs reads REPO_RECALL_VERSION
    # so the installed binary reports the tag the formula was built from.
    ENV["REPO_RECALL_VERSION"] = version.to_s
    system "cargo", "install", *std_cargo_args
    pkgshare.install "static"
  end

  service do
    run [opt_bin/"repo-recall"]
    keep_alive true
    working_dir "#{Dir.home}/projects/coilysiren"
    log_path var/"log/repo-recall.log"
    error_log_path var/"log/repo-recall.err.log"
    environment_variables(
      REPO_RECALL_CWD: "#{Dir.home}/projects/coilysiren",
      REPO_RECALL_PORT: "7777",
      REPO_RECALL_DEPTH: "4",
      REPO_RECALL_STATIC: opt_pkgshare/"static",
      PATH: "#{HOMEBREW_PREFIX}/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
    )
  end

  def caveats
    <<~EOS
      To configure repo-recall (working directory, port, scan depth, etc.):
        brew services edit repo-recall
        brew services restart repo-recall
      Edits to the service file persist across `brew upgrade`.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/repo-recall --version")
    assert_match "repo-recall", shell_output("#{bin}/repo-recall --help")
  end
end
