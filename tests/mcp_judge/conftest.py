def pytest_addoption(parser):
    parser.addoption("--record", action="store_true", help="record the current Fabric MCP server transcripts")


def pytest_configure(config):
    if config.getoption("--record"):
        import os
        os.environ["MCP_JUDGE_RECORD"] = "1"
