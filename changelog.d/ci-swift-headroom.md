- Reclaim the unused preinstalled Swift SDK on GitHub-hosted Ubuntu validation runners to
  provide additional build headroom. Keep the 40 GiB preparation requirement, 10 GiB production
  free-disk floor and existing cleanup safety checks unchanged; local gates never remove SDKs.
