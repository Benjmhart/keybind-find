# Kakoune's own report of its effective mappings for the kakoune-oracle case, run as
# `kak -ui dummy -e "source $E/oracle-kakoune.kak"` (a client, so its window's maps count too): a marker, its version
# and `debug mappings`, then the whole *debug* buffer to stdout, and the session ends.
echo -debug KEYBIND-FIND-ORACLE
echo -debug "version: %val{version}"
debug mappings
evaluate-commands -no-hooks -buffer *debug* %{
    execute-keys '%'
    echo -to-file /dev/stdout %val{selection}
}
kill!
