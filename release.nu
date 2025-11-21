#!/usr/bin/env nu

use std assert

export def main [version: string] {   
    # make sure the working tree is clean
    assert_repo_is_clean

    let read_version = version_read_cargo_toml
    let requested_version = $version | version_parse
    let requested_version_tag = $requested_version | version_to_string

    $"Updating version from ($read_version | version_to_string) to ($requested_version | version_to_string)" | echo_section_headline

    if not (is_newer $read_version $requested_version) {
        print "Requested version is older than current version. Aborting."
        exit 1
    }

    # update the cargo toml
    patch_cargo_toml $requested_version

    # update cargo lock
    update_cargo_lock

    # replace NEXTRELEASE with version
    update_next_release src/command_line.rs $version
    update_next_release src/configuration.rs $version
    update_next_release README.md $version

    # show diff so we can review the replacements
    git_diff

    if ((input "Proceed with commit? (Y/n) " --numchar 1 --default "Y") | str downcase) == "n" {
        exit 1
    }

    # commit 
    git_commit $requested_version_tag

    # tag a new git version
    git_tag $requested_version_tag

    ## from here on we go online!
    # push
    git_push 

    # the rest is being handled by the github release action
}

def echo_section_headline []: string -> nothing {
    print $"\n(ansi yellow)++ ($in)(ansi reset)"
}

def assert_repo_is_clean [] {
    if (git diff --quiet | complete | get exit_code) != 0 {
        print "The git repository is not clean! Aborting..."
        exit 1
    } else {}
}

def git_diff [] {
    git --no-pager diff
}

def git_tag [tag: string] {
    assert_repo_is_clean

    $"Creating Git Tag ($tag) " | echo_section_headline
    git tag ($tag)
}

def git_push [] {
    "Pushing to GitHub" | echo_section_headline
    git push; git push --tags
}

def patch_cargo_toml [version: list<int>] {
    "Updating Cargo.toml" | echo_section_headline
    let sed_string = $"/package/,/version =/{s/version.*/version = \"($version | str join '.')\"/}"
    
    sed -i $sed_string Cargo.toml
}

def update_cargo_lock [] {
    "Updating Cargo.lock" | echo_section_headline
    cargo generate-lockfile
}

def update_next_release [filename: string, version: string] {
    sed -i -e $'s,NEXTRELEASE,($version),g' $filename
}

def git_commit [tag: string] {
    "Committing..." | echo_section_headline
    git commit -am $"Updating version to ($tag)"
}

def version_parse []: string -> list<int> {
    $in | str trim -c 'v' --left | split row '.' | each {|n| into int }
}

def version_to_string []: list<int> -> string {
    $"v($in | str join '.')"
}

def version_read_cargo_toml []: nothing -> list<int> {
    open Cargo.toml | get package.version | version_parse
}

def is_newer [
    old: list<int>,
    new: list<int>
    ]: nothing -> bool {
    
    let length = [($old | length) ($new | length)] | math min

    for i in 0..<($length) {
        if ($new | get $i) > ($old | get $i) {
            return true
        } else {}
        if ($new | get $i) < ($old | get $i) {
            return false
        } else {}
    }

    return false
}

#[test]
def test_versions [] {
    assert (is_newer [1 0 0] [2 0 0]) "major version"
    assert (is_newer [1 0 0] [1 1]) "minor version, shorter"
    assert not (is_newer [1 1 0] [1 1]) "minor version, shorter"
    assert not (is_newer [1 1 0] [0 1]) "minor version, shorter"
}
