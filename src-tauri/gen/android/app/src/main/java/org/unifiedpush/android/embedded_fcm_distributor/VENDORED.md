UnifiedPush embedded FCM distributor 3.1.0, unchanged, from
https://codeberg.org/UnifiedPush/android-embedded_fcm_distributor (Apache License 2.0,
https://www.apache.org/licenses/LICENSE-2.0).

Vendored rather than depended on: the published artifact needs Kotlin 2.3's stdlib, which
this project's Kotlin 1.9 compiler can't read. Here it builds with the project's own
compiler and kotlinx-coroutines 1.8.
