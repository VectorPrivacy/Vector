plugins {
    id("com.android.library") version "8.3.0"
    id("org.jetbrains.kotlin.android") version "1.9.22"
    `maven-publish`
}

android {
    namespace = "build.marmot.mdk"
    compileSdk = 34

    defaultConfig {
        minSdk = 21

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        consumerProguardFiles("consumer-rules.pro")
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    kotlinOptions {
        jvmTarget = "1.8"
    }

    publishing {
        singleVariant("release") {
            withSourcesJar()
            withJavadocJar()
        }
    }
}

dependencies {
    implementation("net.java.dev.jna:jna:5.14.0@aar")
    implementation("androidx.annotation:annotation:1.7.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.7.3")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.5")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.1")
}

// Read version from gradle.properties (set during build from Cargo.toml)
// Falls back to a default if not set (for standalone builds)
val libraryVersion: String = project.findProperty("libraryVersion") as String? 
    ?: throw GradleException("libraryVersion property not found in gradle.properties. This should be set during the build process from Cargo.toml.")

publishing {
    publications {
        register<MavenPublication>("release") {
            groupId = "org.parres"
            artifactId = "mdk"
            version = libraryVersion

            afterEvaluate {
                from(components["release"])
            }
        }
    }
    repositories {
        maven {
            name = "myRepo"
            url = uri(layout.buildDirectory.dir("repo"))
        }
    }
}

