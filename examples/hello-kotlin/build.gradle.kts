// The hello campaign in Kotlin. The client code is generated from the
// nanoprobe schema at build time by the Gradle protobuf plugin with the
// grpc-java and grpc-kotlin generators; nothing generated is checked in.
import com.google.protobuf.gradle.id

plugins {
    kotlin("jvm") version "2.4.21"
    id("com.google.protobuf") version "0.10.1"
    application
}

repositories { mavenCentral() }

val grpc = "1.85.1"
val grpcKotlin = "1.5.0"
val protobuf = "4.36.2"

dependencies {
    // grpc-kotlin-stub pulls an older grpc-stub; the BOM keeps every grpc-java
    // artifact at the version of the code generator.
    implementation(platform("io.grpc:grpc-bom:$grpc"))
    implementation("io.grpc:grpc-netty-shaded")
    implementation("io.grpc:grpc-protobuf")
    implementation("io.grpc:grpc-stub")
    implementation("io.grpc:grpc-kotlin-stub:$grpcKotlin")
    implementation("com.google.protobuf:protobuf-kotlin:$protobuf")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.11.0")
}

kotlin { jvmToolchain(21) }

// The schema. A campaign's own project unpacks the release's schema archive and
// names its proto/ folder here; this example uses the repository's.
sourceSets {
    main { proto { srcDir(providers.gradleProperty("nanoprobe.proto").getOrElse("../../proto")) } }
}

protobuf {
    protoc { artifact = "com.google.protobuf:protoc:$protobuf" }
    plugins {
        id("grpc") { artifact = "io.grpc:protoc-gen-grpc-java:$grpc" }
        id("grpckt") { artifact = "io.grpc:protoc-gen-grpc-kotlin:$grpcKotlin:jdk8@jar" }
    }
    generateProtoTasks {
        all().forEach {
            it.plugins {
                id("grpc")
                id("grpckt")
            }
            it.builtins { id("kotlin") }
        }
    }
}

application { mainClass = "HelloKt" }
