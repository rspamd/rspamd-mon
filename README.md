# Rspamd monitor

[![CI](https://github.com/rspamd/rspamd-mon/actions/workflows/check_and_lint.yml/badge.svg)](https://github.com/rspamd/rspamd-mon/actions/workflows/check_and_lint.yml)

This project is a simple CLI utility to monitor runtime Rspamd instance stat via the controller HTTP interface.
It is written in Rust just for fun.

Build:

`cargo build`

Usage:

```
USAGE:
    rspamd-mon [OPTIONS]

OPTIONS:
        --chart-height <CHART_HEIGHT>    Chart height [default: 6]
        --chart-width <CHART_WIDTH>      Chart width [default: 80]
    -h, --help                           Print help information
        --timeout <TIMEOUT>              How often do we poll Rspamd [default: 1.0]
        --url <url>                      [default: http://localhost:11334/stat]
    -v, --verbose                        Verbosity level: -v - info, -vv - debug, -vvv - trace
```

![Screenshot](<assets/screenshot.png?raw=true>)

This project was inspired by code from [@sandreim](https://github.com/sandreim), so thanks Andrei :)
