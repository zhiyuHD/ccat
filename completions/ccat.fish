complete -c ccat -l completions -d 'Generate shell completions' -r -f -a "bash\t''
elvish\t''
fish\t''
powershell\t''
zsh\t''"
complete -c ccat -s D -l diff -d 'Diff mode: compare two files (like `diff`)' -r
complete -c ccat -s e -l edit -d 'Apply sed-like substitution (e.g. s/foo/bar/)' -r
complete -c ccat -s A -l ascii -d 'Force plain text output'
complete -c ccat -s B -l binary -d 'Display raw bytes (no processing)'
complete -c ccat -s T -l type -d 'Show detected file type (like `file` command)'
complete -c ccat -s n -l number -d 'Number lines (-n: all, -b: non-blank)'
complete -c ccat -s b -l number-nonblank -d 'Number non-blank lines'
complete -c ccat -s s -l squeeze-blank -d 'Squeeze consecutive blank lines into one'
complete -c ccat -s h -l help -d 'Print help'
complete -c ccat -s V -l version -d 'Print version'
