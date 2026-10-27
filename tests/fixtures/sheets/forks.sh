#!/bin/sh
# Prints a sheet, exits, and leaves a child holding stdout open.
sleep 4.73 &
echo '{"title":"t","provider":"x","binds":{}}'
