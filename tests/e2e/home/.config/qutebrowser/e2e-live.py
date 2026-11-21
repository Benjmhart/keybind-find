# The qutebrowser-live case's config (--config-py): autoconfig.yml, and ,l, which only the running browser has.
config.load_autoconfig()
config.bind(',l', 'message-info "e2e live"')
