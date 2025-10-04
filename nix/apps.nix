# Flake apps definitions
# This module contains:
# - Application entry points
# - CI/CD utility apps
# - Development utility apps
# - Cache management apps

{ flake-utils
, harness
, binaryCacheUtils
, devUtils
, cacheUtils
}:

{
  default = flake-utils.lib.mkApp {
    drv = harness;
    exePath = "/bin/harness";
  };

  harness = flake-utils.lib.mkApp {
    drv = harness;
    exePath = "/bin/harness";
  };

  # CI/CD utilities
  ci-cache-optimize = flake-utils.lib.mkApp {
    drv = binaryCacheUtils.ci-cache-optimize;
  };

  container-test = flake-utils.lib.mkApp {
    drv = devUtils.container-test;
  };

  cache-analytics = flake-utils.lib.mkApp {
    drv = binaryCacheUtils.cache-analytics;
  };

  push-cache = flake-utils.lib.mkApp {
    drv = binaryCacheUtils.push-cache;
  };

  # Development utilities
  dev-test = flake-utils.lib.mkApp {
    drv = devUtils.dev-test;
  };

  dev-build = flake-utils.lib.mkApp {
    drv = devUtils.dev-build;
  };

  # Cache management
  cache-info = flake-utils.lib.mkApp {
    drv = cacheUtils.cache-info;
  };
}
