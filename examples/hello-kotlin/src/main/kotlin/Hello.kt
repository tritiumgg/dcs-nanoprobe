// The hello campaign in Kotlin: the smallest program that runs a campaign
// through the nanoprobe API. It does what examples/hello-rust does.
//
// It takes the controller lease, creates and activates a campaign, puts one
// ground group in it with a route, and watches the world until the group is
// live in DCS and its lead has reached the route's end, renewing its lease at a
// third of the lease's time as it goes. Then it gives the lease up.
//
// The server's URL is the first argument, or NANOPROBE_ADDR, or
// http://127.0.0.1:47311. A token, if the server needs one, is read from
// NANOPROBE_TOKEN.

import io.grpc.ManagedChannel
import io.grpc.ManagedChannelBuilder
import io.grpc.Metadata
import io.grpc.StatusException
import io.grpc.stub.MetadataUtils
import java.net.URI
import java.util.concurrent.TimeUnit
import kotlin.math.hypot
import kotlin.system.exitProcess
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.firstOrNull
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import nanoprobe.v1alpha1.CampaignServiceGrpcKt.CampaignServiceCoroutineStub
import nanoprobe.v1alpha1.CommandsServiceGrpcKt.CommandsServiceCoroutineStub
import nanoprobe.v1alpha1.EntityState
import nanoprobe.v1alpha1.Event
import nanoprobe.v1alpha1.EventsServiceGrpcKt.EventsServiceCoroutineStub
import nanoprobe.v1alpha1.GroupCategory
import nanoprobe.v1alpha1.Position
import nanoprobe.v1alpha1.RateClass
import nanoprobe.v1alpha1.SessionServiceGrpcKt.SessionServiceCoroutineStub
import nanoprobe.v1alpha1.WorldChange
import nanoprobe.v1alpha1.WorldServiceGrpcKt.WorldServiceCoroutineStub
import nanoprobe.v1alpha1.acquireLeaseRequest
import nanoprobe.v1alpha1.createCampaignRequest
import nanoprobe.v1alpha1.eventFilter
import nanoprobe.v1alpha1.groupSpec
import nanoprobe.v1alpha1.helloRequest
import nanoprobe.v1alpha1.idempotency
import nanoprobe.v1alpha1.listWorldRequest
import nanoprobe.v1alpha1.order
import nanoprobe.v1alpha1.position
import nanoprobe.v1alpha1.putGroupRequest
import nanoprobe.v1alpha1.releaseLeaseRequest
import nanoprobe.v1alpha1.renewLeaseRequest
import nanoprobe.v1alpha1.routeOrder
import nanoprobe.v1alpha1.subscribeEventsRequest
import nanoprobe.v1alpha1.unitSpec
import nanoprobe.v1alpha1.watchWorldRequest
import nanoprobe.v1alpha1.waypoint
import nanoprobe.v1alpha1.worldFilter
import nanoprobe.v1alpha1.write

// Where the hello campaign runs: the theater, a start and an end.
const val THEATER = "Caucasus"
val START = Pair(-290_000.0, 680_000.0)
val END = Pair(-289_000.0, 680_000.0)
// How close the lead must come to the end, meters.
const val ARRIVED_M = 50.0
// The unit type of both units, and the country of the group.
const val UNIT_TYPE = "M-113"
const val COUNTRY = 2
// Fails after this long, milliseconds.
const val LIMIT_MS = 300_000L
// The shortest wait between lease renewals, milliseconds.
const val MIN_RENEW_MS = 10L
const val SCHEMA_VERSION = "v1alpha1"
const val CLIENT_NAME = "hello-kotlin"
const val DEFAULT_ADDR = "http://127.0.0.1:47311"

/** A failure the program reports and exits 1 on. */
class HelloError(message: String) : Exception(message)

/** An enum value's name without its prefix, in title case: Live. */
fun short(name: String, prefix: String): String =
    name.removePrefix(prefix).lowercase().replaceFirstChar { it.uppercase() }

fun at(north: Double, east: Double): Position = position {
    theater = THEATER
    this.north = north
    this.east = east
}

/** Run a call, turning a gRPC error into `What: CODE message`. */
suspend fun <T> call(what: String, block: suspend () -> T): T =
    try {
        block()
    } catch (e: StatusException) {
        throw HelloError("$what: ${e.status.code} ${e.status.description ?: ""}")
    }

/** A plaintext channel to an http:// URL, with the token as a bearer header. */
fun channel(endpoint: String, token: String?): ManagedChannel {
    val url = URI(if ("://" in endpoint) endpoint else "http://$endpoint")
    if (url.scheme != "http" || url.host == null) {
        throw HelloError("endpoint: only http:// URLs are supported: $endpoint")
    }
    val builder = ManagedChannelBuilder.forAddress(url.host, if (url.port < 0) 80 else url.port)
        .usePlaintext()
    if (token != null) {
        val headers = Metadata()
        headers.put(Metadata.Key.of("authorization", Metadata.ASCII_STRING_MARSHALLER), "Bearer $token")
        builder.intercept(MetadataUtils.newAttachHeadersInterceptor(headers))
    }
    return builder.build()
}

/** What the run saw. */
data class Summary(var campaign: Long = 0, var group: Long = 0, var revision: Long = 0, var events: Int = 0)

suspend fun run(channel: ManagedChannel): Summary {
    val session = SessionServiceCoroutineStub(channel)
    val campaigns = CampaignServiceCoroutineStub(channel)
    val commands = CommandsServiceCoroutineStub(channel)
    val world = WorldServiceCoroutineStub(channel)
    val events = EventsServiceCoroutineStub(channel)

    val hello = call("Hello") {
        session.hello(helloRequest {
            clientName = CLIENT_NAME
            schemaVersion = SCHEMA_VERSION
        })
    }
    println("server ${hello.serverVersion} schema ${hello.schemaVersion} role ${short(hello.role.name, "ROLE_")}")

    val granted = call("AcquireLease") { session.acquireLease(acquireLeaseRequest { clientName = CLIENT_NAME }) }
    val lease = granted.lease
    if (!granted.hasTtl() || granted.ttl.seconds < 0 || granted.ttl.nanos < 0) {
        throw HelloError("AcquireLease returned no lease time")
    }
    val ttlMs = granted.ttl.seconds * 1000 + granted.ttl.nanos / 1_000_000
    val renewEveryMs = maxOf(ttlMs / 3, MIN_RENEW_MS)
    println("lease epoch ${lease.epoch}")

    // A name per run, so a second run on the same server makes its own.
    val name = "hello-${System.currentTimeMillis()}"
    fun key(what: String) = idempotency { key = "hello/$name/$what" }
    val created = call("CreateCampaign") {
        campaigns.createCampaign(createCampaignRequest {
            this.lease = lease
            this.name = name
            theaters += THEATER
            firstTheater = THEATER
            activate = true
            idempotency = key("create")
        })
    }
    if (!created.hasCampaign()) throw HelloError("CreateCampaign returned no campaign")
    val campaign = created.campaign
    println("campaign ${campaign.id} `${campaign.name}`")
    val summary = Summary(campaign = campaign.id)

    fun unit(label: String, east: Double) = unitSpec {
        type = UNIT_TYPE
        this.label = label
        position = at(START.first, START.second + east)
    }
    val put = call("PutGroup") {
        commands.putGroup(putGroupRequest {
            write = write {
                this.lease = lease
                idempotency = key("convoy")
                campaignId = campaign.id
            }
            groupId = 0
            spec = groupSpec {
                label = "convoy"
                country = COUNTRY
                category = GroupCategory.GROUP_CATEGORY_GROUND
                units += unit("lead", 0.0)
                units += unit("second", 20.0)
            }
            order = order {
                route = routeOrder {
                    waypoints += waypoint {
                        position = at(END.first, END.second)
                        speed = 10.0
                        onRoad = false
                    }
                }
            }
        })
    }
    println("group ${put.groupId} units ${put.unitIdsList} at revision ${put.revision}")
    val lead = put.unitIdsList.firstOrNull() ?: throw HelloError("PutGroup returned no units")
    summary.group = put.groupId

    // List, then watch from the listed revision.
    val filter = worldFilter { campaignId = campaign.id }
    val listed = call("ListWorld") { world.listWorld(listWorldRequest { this.filter = filter }).toList() }
    val revision = listed.lastOrNull()?.revision ?: 0

    coroutineScope {
        // Both end only on an error, or when the world task below is done.
        val renew = launch {
            while (true) {
                delay(renewEveryMs)
                call("RenewLease") { session.renewLease(renewLeaseRequest { this.lease = lease }) }
            }
        }
        // Events from the start of the campaign's log.
        val follow = launch {
            call("SubscribeEvents") {
                events.subscribeEvents(subscribeEventsRequest {
                    this.filter = eventFilter { campaignId = campaign.id }
                    afterSequence = 0
                    liveOnly = false
                }).collect { printEvent(it.event); summary.events += 1 }
            }
            throw HelloError("the event stream ended")
        }

        var live = false
        var arrived = false
        var lastState: String? = null
        call("WatchWorld") {
            world.watchWorld(watchWorldRequest {
                this.filter = filter
                sinceRevision = revision
                rate = RateClass.RATE_CLASS_FAST
            }).firstOrNull { m ->
                summary.revision = m.revision
                for (c in m.changesList) {
                    when (c.changeCase) {
                        WorldChange.ChangeCase.UPSERT -> {
                            if (!c.upsert.hasGroup() || c.upsert.group.id != put.groupId) continue
                            val g = c.upsert.group
                            val s = short(g.state.name, "ENTITY_STATE_")
                            if (s != lastState) {
                                println("group ${g.id} is $s at revision ${m.revision}")
                                lastState = s
                            }
                            live = g.state == EntityState.ENTITY_STATE_LIVE
                        }
                        WorldChange.ChangeCase.POSITIONS -> {
                            val b = c.positions
                            for (i in 0 until b.unitIdCount) {
                                if (b.getUnitId(i) != lead || arrived) continue
                                val d = hypot(b.getNorth(i) - END.first, b.getEast(i) - END.second)
                                if (d <= ARRIVED_M) {
                                    println("lead $lead reached the end of its route")
                                    arrived = true
                                }
                            }
                        }
                        else -> {}
                    }
                }
                live && arrived
            } ?: throw HelloError("the watch ended")
        }
        renew.cancel()
        follow.cancel()
    }

    call("ReleaseLease") { session.releaseLease(releaseLeaseRequest { this.lease = lease }) }
    println("done: lease released")
    return summary
}

fun printEvent(e: Event) {
    when (e.bodyCase) {
        Event.BodyCase.LIFECYCLE -> println(
            "event ${e.sequence}: sim ${short(e.lifecycle.state.name, "SIM_STATE_")} on `${e.lifecycle.theater}`"
        )
        Event.BodyCase.WORLD -> println("event ${e.sequence}: DCS event ${e.world.dcsId}")
        Event.BodyCase.GAP -> println("event ${e.sequence}: ${e.gap.lost} events lost")
        else -> println("event ${e.sequence}")
    }
}

fun main(args: Array<String>) {
    val endpoint = args.firstOrNull() ?: System.getenv("NANOPROBE_ADDR") ?: DEFAULT_ADDR
    val token = System.getenv("NANOPROBE_TOKEN")
    val code = runBlocking {
        var ch: ManagedChannel? = null
        try {
            ch = channel(endpoint, token)
            val s = withTimeout(LIMIT_MS) { run(ch) }
            println("hello campaign done: campaign ${s.campaign} group ${s.group} revision ${s.revision} events ${s.events}")
            0
        } catch (e: TimeoutCancellationException) {
            System.err.println("$CLIENT_NAME: the group did not arrive within ${LIMIT_MS / 1000}s")
            1
        } catch (e: HelloError) {
            System.err.println("$CLIENT_NAME: ${e.message}")
            1
        } finally {
            ch?.shutdownNow()?.awaitTermination(5, TimeUnit.SECONDS)
        }
    }
    exitProcess(code)
}
