.. _packaging:

====================
Packaging User Guide
====================

So you want to package a Python application using PyOxidizer? You've come
to the right place to learn how! Read on for all the details on how to
*oxidize* your Python application!

First, you'll need to install PyOxidizer. See :ref:`installing` for
instructions.

Creating a PyOxidizer Project
=============================

Behind the scenes, PyOxidizer works by creating a Rust project which embeds
and runs a Python interpreter.

The process for *oxidizing* every Python application looks the same: you
start by creating a new [Rust] project with the PyOxidizer scaffolding.
The ``pyoxidizer init`` command does this::

   # Create a new project named "pyapp" in the directory "pyapp"
   $ pyoxidizer init pyapp

   # Create a new project named "myapp" in the directory "~/src/myapp"
   $ pyoxidizer init ~/src/myapp

The default project created by ``pyoxidizer init`` will produce an executable
that embeds Python and starts a Python REPL. Let's test that::

   $ pyoxidizer run pyapp
   no existing PyOxidizer artifacts found
   processing config file /home/gps/src/pyapp/pyoxidizer.bzl
   resolving Python distribution...
      Compiling pyapp v0.1.0 (/home/gps/src/pyapp)
       Finished dev [unoptimized + debuginfo] target(s) in 53.14s
        Running `target/debug/pyapp`
   >>>

If all goes according to plan, you just built a Rust executable which
contains an embedded copy of Python. That executable started an interactive
Python debugger on startup. Try typing in some Python code::

   >>> print("hello, world")
   hello, world

It works!

(To exit the REPL, press CTRL+d or CTRL+z or ``import sys; sys.exit(0)`` from
the REPL.)

.. note::

   If you have built a Rust project before, the output from building a
   PyOxidizer application may look familiar to you. That's because under the
   hood Cargo - Rust's package manager and build system - is doing a lot of the
   work to build the application. If you are familiar with Rust development,
   feel free to use ``cargo build`` and ``cargo run`` directly. However, Rust's
   build system is only responsible for some functionality. Most notable,
   all the post-build *packaging* steps such as copying binaries to the
   ``build/apps`` directory is not performed by the Rust build system, so
   built applications may be incomplete.

If you are curious about what's inside newly-created projects, read
:ref:`new_project_layout`.

Now that we've got a new project, let's customize it to do something useful.

Packaging an Application from a PyPI Package
============================================

In this section, we'll show how to package the
`pyflakes <https://pypi.org/project/pyflakes/>`_ program using a published
PyPI package. (Pyflakes is a Python linter.)

First, let's create an empty project::

   $ pyoxidizer init pyflakes

Next, we need to edit the :ref:`configuration file <config_files>` to tell
PyOxidizer about pyflakes. Open the ``pyflakes/pyoxidizer.bzl`` file in your
favorite editor.

We first tell PyOxidizer to add the ``pyflakes`` Python package by adding the
following lines:

.. code-block:: python

   install_pyflakes = PipInstallSimple("pyflakes==2.1.1")

This creates a packaging rule that essentially translates to running
``pip install pyflakes==2.1.1`` and then finds and packages the files installed
by that command.

Next, we tell PyOxidizer to run pyflakes when the interpreter is executed.

.. code-block:: python

   python_run_mode = python_run_mode_eval("from pyflakes.api import main; main()")

This says to effectively run the Python code
``eval(from pyflakes.api import main; main())`` when the embedded interpreter
starts.

The new ``pyoxidizer.bzl`` file should look something like:

.. code-block:: python

   embedded_python_config = EmbeddedPythonConfig()
   stdlib_extensions_policy = StdlibExtensionsPolicy("all")
   stdlib = Stdlib(include_source=False)
   install_pyflakes = PipInstallSimple("pyflakes==2.1.1")
   python_run_mode = python_run_mode_eval("from pyflakes.api import main; main()")

   Config(
       application_name="pyflakes",
       embedded_python_config=embedded_python_config,
       python_distribution=default_python_distribution(),
       python_run_mode=python_run_mode,
       packaging_rules=[stdlib, stdlib_extensions_policy, install_pyflakes],
   )

With the configuration changes made, we can build and run a ``pyflakes``
native executable::

   # From outside the ``pyflakes`` directory
   $ pyoxidizer run /path/to/pyflakes/project -- /path/to/python/file/to/analyze

   # From inside the ``pyflakes`` directory
   $ pyoxidizer run -- /path/to/python/file/to/analyze

   # Or if you prefer the Rust native tools
   $ cargo run -- /path/to/python/file/to/analyze

By default, ``pyflakes`` analyzes Python source code passed to it via
stdin.

What Can Go Wrong
=================

Ideally, packaging your Python application and its dependencies *just works*.
Unfortunately, we don't live in an ideal world.

PyOxidizer breaks various assumptions about how Python applications are
built and distributed. When attempting to package your application, you will
inevitably run into problems due to incompatibilities with PyOxidizer.

The :ref:`pitfalls` documentation can serve as a guide to identify and work
around these problems.

Packaging Additional Files
==========================

By default PyOxidizer will embed Python resources such as modules into
the compiled executable. This is the ideal method to produce distributable
Python applications because it can keep the entire application self-contained
to a single executable and can result in
:ref:`performance wins <better_performance>`.

But sometimes embedded resources into the binary isn't desired or doesn't
work. Fear not: PyOxidizer has you covered!

As documented at :ref:`install_locations`, many packaging rules in PyOxidizer
configuration files can define an ``install_location`` that denotes where
resources found by a packaging rule are installed.

Let's give an example of this by attempting to package
`black <https://github.com/python/black>`_, a Python code formatter.

We start by creating a new project::

   $ pyoxidizer init black

Then edit the ``pyoxidizer.bzl`` file to have the following:

.. code-block:: python

   embedded_python_config = EmbeddedPythonConfig()
   stdlib_extensions_policy = StdlibExtensionsPolicy("all")
   stdlib = Stdlib(include_source=False)
   install_black = PipInstallSimple("black==19.3b0")
   python_run_mode = python_run_mode_module("black")

   Config(
       application_name="black",
       embedded_python_config=embedded_python_config,
       python_distribution=default_python_distribution(),
       python_run_mode=python_run_mode,
       packaging_rules=[stdlib, stdlib_extensions_policy, install_black],
   )

Then let's attempt to build the application::

   $ pyoxidizer build black
   processing config file /home/gps/src/black/pyoxidizer.bzl
   resolving Python distribution...
   ...
   packaging application into /home/gps/src/black/build/apps/x86_64-unknown-linux-gnu/debug/black
   purging /home/gps/src/black/build/apps/black/x86_64-unknown-linux-gnu/debug
   copying /home/gps/src/black/build/target/x86_64-unknown-linux-gnu/debug/black to /home/gps/src/black/build/apps/black/x86_64-unknown-linux-gnu/debug/black
   resolving packaging state...
   black packaged into /home/gps/src/black/build/apps/black/x86_64-unknown-linux-gnu/debug

Looking good so far!

Now let's try to run it::

   $  black/build/apps/black/x86_64-unknown-linux-gnu/debug/black
   Traceback (most recent call last):
     File "black", line 46, in <module>
     File "blib2to3.pygram", line 15, in <module>
   NameError: name '__file__' is not defined
   SystemError

Uh oh - that's didn't work as expected.

As the error message shows, the ``blib2to3.pygram`` module is trying to
access ``__file__``, which is not defined. As explained by :ref:`no_file`,
PyOxidizer doesn't set ``__file__`` for modules loaded from memory. This is
perfectly legal as Python doesn't mandate that ``__file__`` be defined. So
``black`` (and every other Python file assuming the existence of ``__file__``)
is buggy.

Let's assume we can't easily change the offending source code.

To fix this problem, we change the packaging rule to install ``black``
relative to the built application.

Simply change the following rule:

.. code-block:: python

   install_black = PipInstallSimple("black==19.3b0")

To:

.. code-block:: python

   install_black = PipInstallSimple("black=19.3b0", install_location="app-relative:lib")

The added ``install_location="app-relative:lib"`` line says to set the
installation location for resources found by that rule to a ``lib``
directory next to the built application.

In addition, we will also need to adjust the ``EmbeddedPythonConfig``
section to have the following:

.. code-block:: python

   embedded_python_config = EmbeddedPythonConfig(sys_paths=["$ORIGIN/lib"])

The added ``sys_paths=["$ORIGIN/lib"]`` line says to populate Python's
``sys.path`` list with a single entry which resolves to a ``lib`` sub-directory
in the executable's directory. This configuration change is necessary to allow
the Python interpreter to import Python modules from the filesystem and to find
the modules that our packaging rule installed into the ``lib`` directory.

Now let's re-build the application::

   $ pyoxidizer build black
   ...
   packaging application into /home/gps/src/black/build/apps/black/x86_64-unknown-linux-gnu/debug
   purging /home/gps/src/black/build/apps/black/x86_64-unknown-linux-gnu/debug
   copying /home/gps/src/black/build/target/x86_64-unknown-linux-gnu/debug/black to /home/gps/src/black/build/apps/black/x86_64-unknown-linux-gnu/debug/black
   resolving packaging state...
   installing resources into 1 app-relative directories
   installing 46 app-relative Python source modules to /home/gps/src/black/build/apps/black/x86_64-unknown-linux-gnu/debug/lib
   ...
   black packaged into /home/gps/src/black/build/apps/black/x86_64-unknown-linux-gnu/debug

If you examine the output, you'll see that various Python modules files were
written to the ``black/build/apps/black/x86_64-unknown-linux-gnu/debug/lib`` directory, just
as our packaging rules requested!

Let's try to run the application::

   $  black/build/apps/black/x86_64-unknown-linux-gnu/debug/black
   No paths given. Nothing to do 😴

Success!

Trimming Unused Resources
=========================

By default, packaging rules are very aggressive about pulling in
resources such as Python modules. For example, the entire Python standard
library is embedded into the binary by default. These extra resources take up
space and can make your binary significantly larger than it could be.

It is often desirable to *prune* your application of unused resources. For
example, you may wish to only include Python modules that your application
uses. This is possible with PyOxidizer.

Essentially, all strategies for managing the set of packaged resources
boil down to crafting packaging ruless that choose which resources
are packaged.

The recommended method to manage resources is the :ref:`rule_filter-include`
packaging rule. This rule acts as an *allow list* filter against all
resources identified for packaging. Using this rule, you can construct an
explicit list of resources that should be packaged.

But maintaining explicit lists of resources can be tedious. There's a better
way!

The :ref:`config_embedded_python_config` config section defines a
``write_modules_directory_env`` setting, which when enabled will instruct
the embedded Python interpreter to write the list of all loaded modules
into a randomly named file in the directory identified by the environment
variable defined by this setting. For example, if you set
``write_modules_directory_env = "PYOXIDIZER_MODULES_DIR"`` and then
run your binary with ``PYOXIDIZER_MODULES_DIR=~/tmp/dump-modules``,
each invocation will write a ``~/tmp/dump-modules/modules-*`` file
containing the list of Python modules loaded by the Python interpreter.

One can therefore use ``write_modules_directory_env`` to produce files
that can be referenced in a ``filter-include`` rule's ``files`` and
``glob_files`` settings.

While PyOxidizer doesn't yet automate the process, one could use a two
phase build to *slim* your binary.

In phase 1, a binary is built with all resources and
``write_modules_directory_env`` enabled. The binary is then executed and
``modules-*`` files are written.

In phase 2, the ``filter-include`` rule is enabled and only the modules
used by the instrumented binary will be packaged.

Adding Extension Modules At Run-Time
====================================

Normally, Python extension modules are compiled into the binary as part
of the embedded Python interpreter.

PyOxidizer also supports providing additional extension modules at run-time.
This can be useful for larger Rust applications providing extension modules
that are implemented in Rust and aren't built through normal Python
build systems (like ``setup.py``).

If the ``PythonConfig`` Rust struct used to construct an embedded Python
interpreter contains a populated ``extra_extension_modules`` field, the
extension modules listed therein will be made available to the Python
interpreter.

Please note that Python stores extension modules in a global variable.
So instantiating multiple interpreters via the ``pyembed`` interfaces may
result in duplicate entries or unwanted extension modules being exposed to
the Python interpreter.

Masquerading As Other Packaging Tools
=====================================

Tools to package and distribute Python applications existed several
years before PyOxidizer. Many Python packages have learned to perform
special behavior when the _fingerprint* of these tools is detected at
run-time.

First, PyOxidizer has its own fingerprint: ``sys.oxidized = True``. The
presence of this attribute can indicate an application running with
PyOxidizer.

Since PyOxidizer's run-time behavior is similar to other packaging
tools, PyOxidizer supports falsely identifying itself as these other
tools by emulating their fingerprints.

The ``EmbbedPythonConfig`` configuration section defines the
boolean flag ``sys_frozen`` to control whether ``sys.frozen = True``
is set. This can allow PyOxidizer to advertise itself as a *frozen*
application.

In addition, the ``sys_meipass`` boolean flag controls whether a
``sys._MEIPASS = <exe directory>`` attribute is set. This allows
PyOxidizer to masquerade as having been built with PyInstaller.

.. warning::

   Masquerading as other packaging tools is effectively lying and can
   be dangerous, as code relying on these attributes won't know if
   it is interacting with PyOxidizer or some other tool. It is recommended
   to only set these attributes to unblock enabling packages to
   work with PyOxidizer until other packages learn to check for
   ``sys.oxidized = True``. Setting ``sys._MEIPASS`` is definitely the
   more risky option, as a case can be made that PyOxidizer should set
   ``sys.frozen = True`` by default.
