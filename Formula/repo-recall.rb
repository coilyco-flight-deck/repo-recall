class RepoRecall < Formula
  desc "Local dev dashboard that indexes Claude Code session history against your repos"
  homepage "https://github.com/coilysiren/repo-recall"
  license "MIT"
  head "https://github.com/coilysiren/repo-recall.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  service do
    run [opt_bin/"repo-recall"]
    keep_alive true
    working_dir Dir.home
    log_path var/"log/repo-recall.log"
    error_log_path var/"log/repo-recall.err.log"
    environment_variables(
      REPO_RECALL_CWD: Dir.home,
      REPO_RECALL_PORT: "7777",
      REPO_RECALL_DEPTH: "4",
      PATH: "#{HOMEBREW_PREFIX}/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
    )
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/repo-recall --version")
    assert_match "repo-recall", shell_output("#{bin}/repo-recall --help")
  end
end
