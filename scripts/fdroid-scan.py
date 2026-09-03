#!/usr/bin/env python3
"""Run F-Droid's source scanner over this checkout, the way `fdroid build` would.

    pip install fdroidserver
    scripts/fdroid-scan.py          # after scripts/fdroid-build.sh prepare

Exit status is the problem count. `fdroid scanner` itself insists on cloning
the repo into an fdroid work dir, which is why this drives the library directly.
"""
import argparse
import logging
import os
import shutil
import subprocess
import sys
import tempfile

from fdroidserver import common, metadata, scanner

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
METADATA = os.path.join(ROOT, 'docs', 'fdroid', 'io.vectorapp.yml')


def main():
    logging.basicConfig(level=logging.INFO, format='%(levelname)s: %(message)s')
    work = tempfile.mkdtemp(prefix='fdroid-scan-')
    try:
        os.makedirs(os.path.join(work, 'metadata'))
        shutil.copy(METADATA, os.path.join(work, 'metadata', 'io.vectorapp.yml'))
        # What F-Droid sees: a checkout plus the node_modules `prepare` leaves
        # behind. A dev tree also carries target/, .gradle/ and dist/, which
        # would drown the result in build output the builder never has.
        tree = os.path.join(work, 'build', 'io.vectorapp')
        os.makedirs(tree)
        subprocess.run(
            f'git -C "{ROOT}" ls-files -co --exclude-standard -z | tar -C "{ROOT}" --null -T - -cf - | tar -C "{tree}" -xf -',
            shell=True, check=True,
        )
        node_modules = os.path.join(ROOT, 'node_modules')
        if os.path.isdir(node_modules):
            shutil.copytree(node_modules, os.path.join(tree, 'node_modules'), symlinks=True)
        os.chdir(work)
        common.config = common.read_config()
        common.options = argparse.Namespace(verbose=True, json=False)
        app = metadata.read_metadata()['io.vectorapp']
        build = app['Builds'][-1]
        # scandelete/scanignore act on the tree; this is a read-only check.
        build.scandelete = []
        problems = scanner.scan_source(tree, build)
        print(f'fdroid scanner: {problems} problem(s)')
        return problems
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == '__main__':
    sys.exit(main())
